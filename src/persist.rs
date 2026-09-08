use crate::{
    config::PrinterConfig,
    model::{now, Event, Job, JobState, MediaLedgerEvent, MediaState},
};
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    _writer_lock: Option<Arc<File>>,
}

impl Store {
    pub fn open(root: PathBuf) -> Result<Self> {
        for dir in ["jobs", "payloads", "printers", "spool"] {
            fs::create_dir_all(root.join(dir))?;
        }
        Ok(Self {
            root,
            _writer_lock: None,
        })
    }
    pub fn open_exclusive(root: PathBuf) -> Result<Self> {
        let mut store = Self::open(root)?;
        let lock_path = store.root.join("writer.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening writer lock {}", lock_path.display()))?;
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            anyhow::bail!(
                "data directory {} is already owned by another ZebraTamer process: {error}",
                store.root.display()
            );
        }
        store._writer_lock = Some(Arc::new(lock));
        Ok(store)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn job_path(&self, id: Uuid) -> PathBuf {
        self.root.join("jobs").join(format!("{id}.json"))
    }
    pub fn payload_path(&self, id: Uuid) -> PathBuf {
        self.root.join("payloads").join(format!("{id}.zpl"))
    }
    pub fn spool_path(&self, id: Uuid) -> PathBuf {
        self.root.join("spool").join(format!("{id}.part"))
    }
    pub fn printer_dir(&self, id: &str) -> PathBuf {
        self.root.join("printers").join(id)
    }
    pub fn save_job(&self, job: &Job) -> Result<()> {
        atomic_json(&self.job_path(job.id), job)
    }
    pub fn load_job(&self, id: Uuid) -> Result<Job> {
        read_json(&self.job_path(id))
    }
    pub fn register_job(&self, id: Uuid) -> Result<()> {
        append_ndjson(&self.root.join("jobs.ndjson"), &id)
    }
    pub fn find_job_by_idempotency_key(&self, key: &str, exclude: Uuid) -> Result<Option<Job>> {
        self.find_job_by_idempotency_key_inner(key, Some(exclude))
    }
    pub fn find_job_by_idempotency_key_any(&self, key: &str) -> Result<Option<Job>> {
        self.find_job_by_idempotency_key_inner(key, None)
    }
    pub fn count_active_jobs(&self, printer_id: &str) -> Result<usize> {
        let mut count = 0;
        for entry in fs::read_dir(self.root.join("jobs"))? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(job): Result<Job, _> = read_json(&path) else {
                continue;
            };
            if job.printer_id == printer_id
                && matches!(
                    job.state,
                    JobState::Receiving
                        | JobState::Queued
                        | JobState::Writing
                        | JobState::Verifying
                        | JobState::Held
                )
            {
                count += 1;
            }
        }
        Ok(count)
    }
    fn find_job_by_idempotency_key_inner(
        &self,
        key: &str,
        exclude: Option<Uuid>,
    ) -> Result<Option<Job>> {
        for entry in fs::read_dir(self.root.join("jobs"))? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(job): Result<Job, _> = read_json(&path) else {
                continue;
            };
            if Some(job.id) != exclude
                && job.sha256.is_some()
                && job.idempotency_key.as_deref() == Some(key)
            {
                return Ok(Some(job));
            }
        }
        Ok(None)
    }
    pub fn remove_unregistered_job(&self, id: Uuid) {
        let _ = fs::remove_file(self.job_path(id));
        let _ = fs::remove_file(self.payload_path(id));
        let _ = fs::remove_file(self.spool_path(id));
    }
    pub fn list_jobs_page(&self, cursor: usize, limit: usize) -> Result<Vec<Job>> {
        let mut out = Vec::with_capacity(limit.min(256));
        let index = match File::open(self.root.join("jobs.ndjson")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(error) => return Err(error.into()),
        };
        for line in BufReader::new(index).lines().skip(cursor) {
            if out.len() >= limit {
                break;
            }
            let id: Uuid = serde_json::from_str(&line?)?;
            if let Ok(job) = self.load_job(id) {
                out.push(job);
            }
        }
        Ok(out)
    }
    pub fn recover_jobs(&self) -> Result<Vec<Job>> {
        let mut queued = Vec::new();
        for entry in fs::read_dir(self.root.join("jobs"))? {
            let path = entry?.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(mut job): Result<Job, _> = read_json(&path) else {
                continue;
            };
            if job.state == JobState::Receiving {
                job.state = JobState::Failed;
                job.updated_at = now();
                job.error = Some("agent_restarted_during_upload".into());
                let _ = fs::remove_file(self.spool_path(job.id));
                self.save_job(&job)?;
                continue;
            }
            if matches!(job.state, JobState::Writing | JobState::Verifying) {
                job.state = JobState::OutcomeUnknown;
                job.updated_at = now();
                job.error = Some("agent_restarted_during_delivery".into());
                self.save_job(&job)?;
            } else if job.state == JobState::Queued {
                queued.push(job);
            }
        }
        queued.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(queued)
    }
    pub fn media_path(&self, printer: &str) -> PathBuf {
        self.printer_dir(printer).join("media.json")
    }
    pub fn load_media(&self, printer: &str) -> Result<Option<MediaState>> {
        let p = self.media_path(printer);
        if p.exists() {
            Ok(Some(read_json(&p)?))
        } else {
            Ok(None)
        }
    }
    pub fn save_media(&self, printer: &str, media: &MediaState) -> Result<()> {
        fs::create_dir_all(self.printer_dir(printer))?;
        atomic_json(&self.media_path(printer), media)
    }
    pub fn append_media_ledger(&self, printer: &str, event: &MediaLedgerEvent) -> Result<()> {
        append_ndjson(
            &self.printer_dir(printer).join("media-ledger.ndjson"),
            event,
        )
    }
    pub fn append_event(&self, event: &Event) -> Result<()> {
        append_ndjson(&self.root.join("events.ndjson"), event)
    }
    pub fn next_event(
        &self,
        kind: &str,
        printer: Option<String>,
        job: Option<Uuid>,
        data: Value,
    ) -> Event {
        Event {
            sequence: next_sequence(&self.root.join("events.ndjson")),
            at: now(),
            kind: kind.into(),
            printer_id: printer,
            job_id: job,
            data,
        }
    }
    pub fn read_events(
        &self,
        cursor: u64,
        limit: usize,
        job: Option<Uuid>,
    ) -> Result<(Vec<Event>, u64)> {
        read_ndjson_page(
            &self.root.join("events.ndjson"),
            cursor,
            limit,
            |e: &Event| job.map(|id| e.job_id == Some(id)).unwrap_or(true),
        )
    }
    pub fn boot_id_path(&self) -> PathBuf {
        self.root.join("boot_id")
    }
    pub fn queue_path(&self, printer: &str) -> PathBuf {
        self.printer_dir(printer).join("queue.json")
    }
    pub fn save_queue(&self, printer: &str, queue: &[Uuid]) -> Result<()> {
        fs::create_dir_all(self.printer_dir(printer))?;
        atomic_json(&self.queue_path(printer), &queue)
    }
    pub fn load_queue(&self, printer: &str) -> Result<Vec<Uuid>> {
        let p = self.queue_path(printer);
        if p.exists() {
            read_json(&p)
        } else {
            Ok(vec![])
        }
    }
    pub fn queue_paused_path(&self, printer: &str) -> PathBuf {
        self.printer_dir(printer).join("queue-paused.json")
    }
    pub fn printer_configs_path(&self) -> PathBuf {
        self.root.join("printers.json")
    }
    pub fn load_printer_configs(&self) -> Result<Option<Vec<PrinterConfig>>> {
        let path = self.printer_configs_path();
        if path.exists() {
            Ok(Some(read_json(&path)?))
        } else {
            Ok(None)
        }
    }
    pub fn save_printer_configs(&self, printers: &[PrinterConfig]) -> Result<()> {
        atomic_json(&self.printer_configs_path(), &printers)
    }
    pub fn has_open_jobs(&self, printer_id: &str) -> Result<bool> {
        for entry in fs::read_dir(self.root.join("jobs"))? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(job): Result<Job, _> = read_json(&path) else {
                continue;
            };
            if job.printer_id == printer_id
                && matches!(
                    job.state,
                    JobState::Receiving
                        | JobState::Queued
                        | JobState::Writing
                        | JobState::Verifying
                        | JobState::Held
                )
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn save_queue_paused(&self, printer: &str, paused: bool) -> Result<()> {
        fs::create_dir_all(self.printer_dir(printer))?;
        atomic_json(&self.queue_paused_path(printer), &paused)
    }
    pub fn load_queue_paused(&self, printer: &str) -> Result<bool> {
        let path = self.queue_paused_path(printer);
        if path.exists() {
            read_json(&path)
        } else {
            Ok(false)
        }
    }
}

pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path).with_context(|| format!("atomic rename to {}", path.display()))?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let file = File::open(path)?;
    Ok(serde_json::from_reader(BufReader::new(file))?)
}

pub fn append_ndjson<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    Ok(())
}

pub fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn next_sequence(path: &Path) -> u64 {
    File::open(path)
        .ok()
        .map(|f| BufReader::new(f).lines().count() as u64 + 1)
        .unwrap_or(1)
}

fn read_ndjson_page<T: DeserializeOwned, F: Fn(&T) -> bool>(
    path: &Path,
    cursor: u64,
    limit: usize,
    filter: F,
) -> Result<(Vec<T>, u64)> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((vec![], cursor)),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::with_capacity(limit.min(256));
    let mut next_cursor = cursor;
    for (index, line) in BufReader::new(file).lines().enumerate() {
        if (index as u64) < cursor {
            continue;
        }
        next_cursor = index as u64 + 1;
        let item: T = serde_json::from_str(&line?)?;
        if filter(&item) {
            out.push(item);
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok((out, next_cursor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::ser::Error as _;

    fn job(state: JobState) -> Job {
        Job {
            id: Uuid::new_v4(),
            printer_id: "p1".into(),
            state,
            created_at: now(),
            updated_at: now(),
            label_count: None,
            label_count_source: None,
            origin: None,
            description: None,
            idempotency_key: None,
            sha256: None,
            bytes: 42,
            bytes_transferred: 0,
            delivery_attempts: 0,
            payload_path: None,
            error: None,
        }
    }

    #[test]
    fn recovery_preserves_terminal_jobs_and_never_requeues_in_flight_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let writing = job(JobState::Writing);
        let accepted = job(JobState::TransportAccepted);
        let queued = job(JobState::Queued);
        store.save_job(&writing).unwrap();
        store.save_job(&accepted).unwrap();
        store.save_job(&queued).unwrap();
        let recovered = store.recover_jobs().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].id, queued.id);
        assert_eq!(
            store.load_job(writing.id).unwrap().state,
            JobState::OutcomeUnknown
        );
        assert_eq!(
            store.load_job(accepted.id).unwrap().state,
            JobState::TransportAccepted
        );
    }

    #[test]
    fn recovery_uses_stable_creation_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let mut later = job(JobState::Queued);
        let mut earlier = job(JobState::Queued);
        earlier.created_at = now() - chrono::Duration::seconds(1);
        later.created_at = now();
        store.save_job(&later).unwrap();
        store.save_job(&earlier).unwrap();
        assert_eq!(
            store
                .recover_jobs()
                .unwrap()
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![earlier.id, later.id]
        );
    }

    #[test]
    fn exclusive_store_prevents_a_second_writer() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open_exclusive(dir.path().into()).unwrap();
        assert!(Store::open_exclusive(dir.path().into()).is_err());
        drop(first);
        assert!(Store::open_exclusive(dir.path().into()).is_ok());
    }

    #[test]
    fn atomic_json_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        atomic_json(&path, &serde_json::json!({"ready":true})).unwrap();
        let value: Value = read_json(&path).unwrap();
        assert_eq!(value["ready"], true);
    }

    #[test]
    fn failed_atomic_write_preserves_previous_state_and_removes_temporary_file() {
        struct FailingValue;
        impl Serialize for FailingValue {
            fn serialize<S>(&self, _serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                Err(S::Error::custom("simulated persistence failure"))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        atomic_json(&path, &serde_json::json!({"ready":true})).unwrap();
        assert!(atomic_json(&path, &FailingValue).is_err());
        let value: Value = read_json(&path).unwrap();
        assert_eq!(value["ready"], true);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn printer_configs_round_trip_without_static_config() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        assert!(store.load_printer_configs().unwrap().is_none());

        let printer = PrinterConfig {
            id: "shipping-zebra".into(),
            display_name: "Shipping Zebra".into(),
            transport: "tcp".into(),
            tcp_host: Some("192.0.2.10".into()),
            ..PrinterConfig::default()
        };
        store.save_printer_configs(&[printer]).unwrap();

        let loaded = store.load_printer_configs().unwrap().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "shipping-zebra");
        assert_eq!(loaded[0].tcp_host.as_deref(), Some("192.0.2.10"));
    }

    #[test]
    fn idempotency_lookup_ignores_incomplete_and_excluded_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let mut completed = job(JobState::Queued);
        completed.idempotency_key = Some("service-job-1".into());
        completed.sha256 = Some("abc".into());
        store.save_job(&completed).unwrap();
        let mut receiving = job(JobState::Receiving);
        receiving.idempotency_key = Some("service-job-1".into());
        store.save_job(&receiving).unwrap();

        assert_eq!(
            store
                .find_job_by_idempotency_key("service-job-1", receiving.id)
                .unwrap()
                .unwrap()
                .id,
            completed.id
        );
        assert!(store
            .find_job_by_idempotency_key("service-job-1", completed.id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn active_job_count_excludes_terminal_history_and_other_printers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        for state in [JobState::Receiving, JobState::Queued, JobState::Held] {
            store.save_job(&job(state)).unwrap();
        }
        for state in [
            JobState::TransportAccepted,
            JobState::CompletedObserved,
            JobState::Cancelled,
            JobState::Failed,
            JobState::OutcomeUnknown,
        ] {
            store.save_job(&job(state)).unwrap();
        }
        let mut other = job(JobState::Queued);
        other.printer_id = "p2".into();
        store.save_job(&other).unwrap();
        assert_eq!(store.count_active_jobs("p1").unwrap(), 3);
        assert_eq!(store.count_active_jobs("p2").unwrap(), 1);
    }
}
