use crate::model::{Observed, PrinterSnapshot};

pub fn parse(name: &str, bytes: &[u8], snapshot: &mut PrinterSnapshot) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("empty_response".into());
    }
    let text = clean(bytes);
    match name {
        "host_status" => parse_host_status(&text, snapshot),
        "identification" => parse_identification(&text, snapshot),
        "memory" => parse_memory(&text, snapshot),
        "head_diagnostics" => parse_head_diagnostics(&text, snapshot),
        "configuration" => parse_configuration(&text, snapshot),
        "hq_errors" => parse_hq_errors(&text, snapshot),
        "hq_head_test" => parse_hq_head_test(&text, snapshot),
        "hq_maintenance" => parse_hq_maintenance(&text, snapshot),
        "hq_odometer" => parse_hq_odometer(&text, snapshot),
        "hq_head_life" => parse_hq_head_life(&text, snapshot),
        "hq_plug_and_play" => parse_hq_plug_and_play(&text, snapshot),
        "hq_serial" => parse_hq_serial(&text, snapshot),
        "hq_usb" => parse_hq_usb(&text, snapshot),
        "xml_status" => parse_xml_status(&text, snapshot),
        "sgd_odometer" => parse_sgd_odometer(&text, snapshot),
        _ => Ok(()),
    }
}

fn clean(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace(['\u{0002}', '\u{0003}'], "")
        .replace("<STX>", "")
        .replace("<ETX>", "")
        .replace("<CR>", "\n")
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|line| !line.is_empty())
}

fn parse_u64(value: &str) -> Option<u64> {
    let normalized = value.trim().trim_start_matches('+').replace(',', "");
    let digits: String = normalized
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn parse_i64(value: &str) -> Option<i64> {
    let normalized = value.trim().replace(',', "");
    let digits: String = normalized
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '+')
        .collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn parse_f64(value: &str) -> Option<f64> {
    let normalized = value.trim().replace(',', "");
    let digits: String = normalized
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.'))
        .collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn bool01(value: Option<&&str>) -> Option<bool> {
    match value?.trim() {
        "0" | "N" | "NO" => Some(false),
        "1" | "Y" | "YES" => Some(true),
        _ => None,
    }
}

fn set_bool(target: &mut Observed<bool>, value: Option<bool>, source: &str) {
    if let Some(value) = value {
        *target = Observed::value(value, None, source);
    }
}

fn parse_host_status(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let records: Vec<Vec<&str>> = lines(text)
        .map(|line| line.split(',').map(str::trim).collect())
        .filter(|record: &Vec<&str>| record.len() >= 11)
        .collect();
    if records.len() < 2 || records[0].len() < 11 || records[1].len() < 4 {
        return Err("invalid_~HS_response".into());
    }
    let first = &records[0];
    let second = &records[1];
    set_bool(&mut s.status.media_out, bool01(first.get(1)), "~HS");
    set_bool(&mut s.status.paused, bool01(first.get(2)), "~HS");
    if let Some(value) = first.get(3).and_then(|v| parse_u64(v)) {
        s.settings.label_length_dots = Observed::value(value, Some("dots"), "~HS");
    }
    if let Some(value) = first.get(4).and_then(|v| parse_u64(v)) {
        s.status.formats_buffered = Observed::value(value, Some("formats"), "~HS");
    }
    set_bool(&mut s.status.buffer_full, bool01(first.get(5)), "~HS");
    set_bool(&mut s.status.partial_format, bool01(first.get(7)), "~HS");
    set_bool(
        &mut s.status.corrupt_configuration,
        bool01(first.get(8)),
        "~HS",
    );
    let under = bool01(first.get(9));
    let over = bool01(first.get(10));
    if under.is_some() || over.is_some() {
        s.status.temperature_fault =
            Observed::value(under.unwrap_or(false) || over.unwrap_or(false), None, "~HS");
    }
    set_bool(&mut s.status.head_open, bool01(second.get(1)), "~HS");
    set_bool(&mut s.status.ribbon_out, bool01(second.get(2)), "~HS");
    if let Some(thermal_transfer) = bool01(second.get(3)) {
        s.settings.print_technology = Observed::value(
            if thermal_transfer {
                "thermal_transfer"
            } else {
                "direct_thermal"
            }
            .to_string(),
            None,
            "~HS",
        );
    }
    if let Some(value) = second.get(4) {
        s.status.print_mode = Observed::value((*value).to_string(), None, "~HS");
    }
    if let Some(value) = second.get(7).and_then(|v| parse_u64(v)) {
        s.status.batch_remaining = Observed::value(value, Some("labels"), "~HS");
    }
    if let Some(value) = second.get(9).and_then(|v| parse_u64(v)) {
        s.status.images_stored = Observed::value(value, Some("images"), "~HS");
    }
    let fault = [
        &s.status.paused,
        &s.status.media_out,
        &s.status.ribbon_out,
        &s.status.head_open,
        &s.status.temperature_fault,
    ]
    .iter()
    .any(|v| v.value == Some(true));
    s.status.ready = Observed::value(!fault, None, "~HS");
    Ok(())
}

fn parse_identification(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let fields: Vec<&str> = lines(text)
        .map(|line| line.split(',').map(str::trim).collect::<Vec<_>>())
        .find(|fields| {
            fields.len() >= 4
                && fields[0].chars().any(|character| character.is_alphabetic())
                && parse_u64(fields[2]).is_some()
                && parse_u64(fields[3]).is_some()
        })
        .ok_or("invalid_~HI_response")?;
    let mut model = fields[0].to_string();
    if let Some(pos) = model.rfind('-') {
        if let Some(dpi) = model[pos + 1..].strip_suffix("dpi").and_then(parse_u64) {
            s.identity.resolution_dpi = Observed::value(dpi, Some("dpi"), "~HI");
            model.truncate(pos);
        }
    }
    s.identity.model = Observed::value(model, None, "~HI");
    s.identity.firmware = Observed::value(fields[1].to_string(), None, "~HI");
    if let Some(value) = parse_u64(fields[2]) {
        s.identity.dots_per_mm = Observed::value(value, Some("dots/mm"), "~HI");
    }
    if let Some(value) = parse_u64(fields[3]) {
        s.memory.ram_total_bytes = Observed::value(value * 1024, Some("bytes"), "~HI");
    }
    Ok(())
}

fn parse_memory(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let values: Vec<u64> = lines(text)
        .map(|line| line.split(',').filter_map(parse_u64).collect::<Vec<_>>())
        .find(|values| values.len() == 3)
        .ok_or("invalid_~HM_response")?;
    s.memory.ram_total_bytes = Observed::value(values[0] * 1024, Some("bytes"), "~HM");
    s.memory.ram_maximum_free_bytes = Observed::value(values[1] * 1024, Some("bytes"), "~HM");
    s.memory.ram_free_bytes = Observed::value(values[2] * 1024, Some("bytes"), "~HM");
    Ok(())
}

fn key_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    lines(text).find_map(|line| {
        let (left, right) = line.split_once('=')?;
        (left.trim().eq_ignore_ascii_case(key)).then_some(right.trim())
    })
}

fn parse_head_diagnostics(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let temp = key_value(text, "Head Temp")
        .and_then(parse_f64)
        .ok_or("missing_head_temperature")?;
    s.diagnostics.temperature_celsius = Observed::value(temp, Some("celsius"), "~HD");
    if let Some(value) = key_value(text, "Head Test") {
        s.diagnostics.head_test = Observed::value(value.to_string(), None, "~HD");
    }
    for (key, target) in [
        ("Darkness Adjust", &mut s.settings.darkness),
        ("Print Speed", &mut s.settings.print_speed),
        ("Slew Speed", &mut s.settings.slew_speed),
        ("Backfeed Speed", &mut s.settings.backfeed_speed),
    ] {
        if let Some(value) = key_value(text, key).and_then(parse_f64) {
            *target = Observed::value(
                value,
                if key.contains("Speed") {
                    Some("ips")
                } else {
                    None
                },
                "~HD",
            );
        }
    }
    if let Some(value) = key_value(text, "Dynamic_top_position").and_then(parse_i64) {
        s.settings.label_top_dots = Observed::value(value, Some("dots"), "~HD");
    }
    if let Some(prefixes) = lines(text).find(|line| line.contains("COMMAND PFX")) {
        for part in prefixes.split(':') {
            let part = part.trim();
            if let Some(value) = part.strip_prefix("COMMAND PFX =") {
                s.settings.control_prefix = Observed::value(value.trim().to_string(), None, "~HD");
            } else if let Some(value) = part.strip_prefix("FORMAT PFX =") {
                s.settings.format_prefix = Observed::value(value.trim().to_string(), None, "~HD");
            } else if let Some(value) = part.strip_prefix("DELIMITER =") {
                s.settings.delimiter = Observed::value(value.trim().to_string(), None, "~HD");
            }
        }
    }
    Ok(())
}

fn hh_value<'a>(text: &'a str, label: &str) -> Option<&'a str> {
    lines(text).find_map(|line| {
        let pos = line.rfind(label)?;
        (line[pos..].trim() == label).then(|| line[..pos].trim())
    })
}

fn parse_configuration(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let firmware = hh_value(text, "FIRMWARE").ok_or("missing_firmware")?;
    s.identity.firmware = Observed::value(
        firmware.trim_end_matches("<-").trim().to_string(),
        None,
        "^HH",
    );
    let string_fields = [
        ("HARDWARE ID", &mut s.identity.hardware_id),
        ("SERIAL NUMBER", &mut s.identity.serial_number),
        ("PRINT MODE", &mut s.status.print_mode),
        ("MEDIA TYPE", &mut s.settings.media_type),
        ("SENSOR TYPE", &mut s.settings.sensor_type),
        ("SENSOR SELECT", &mut s.settings.sensor_select),
        ("ZPL MODE", &mut s.settings.zpl_mode),
    ];
    for (label, target) in string_fields {
        if let Some(value) = hh_value(text, label) {
            *target = Observed::value(value.to_string(), None, "^HH");
        }
    }
    for (label, target, unit) in [
        ("PRINT WIDTH", &mut s.settings.print_width_dots, "dots"),
        ("LABEL LENGTH", &mut s.settings.label_length_dots, "dots"),
    ] {
        if let Some(value) = hh_value(text, label).and_then(parse_u64) {
            *target = Observed::value(value, Some(unit), "^HH");
        }
    }
    for (label, target) in [
        ("LABEL TOP", &mut s.settings.label_top_dots),
        ("LEFT POSITION", &mut s.settings.x_offset_dots),
        ("TEAR OFF", &mut s.settings.tear_off_dots),
    ] {
        if let Some(value) = hh_value(text, label).and_then(parse_i64) {
            *target = Observed::value(value, Some("dots"), "^HH");
        }
    }
    if let Some(value) = hh_value(text, "DARKNESS").and_then(parse_f64) {
        s.settings.darkness = Observed::value(value, None, "^HH");
    }
    if let Some(value) = hh_value(text, "PRINT SPEED").and_then(parse_f64) {
        s.settings.print_speed = Observed::value(value, Some("ips"), "^HH");
    }
    if let Some(value) = hh_value(text, "USB COMM.") {
        s.interfaces.usb_connected =
            Observed::value(value.eq_ignore_ascii_case("CONNECTED"), None, "^HH");
    }
    if s.identity.resolution_dpi.value.is_none() {
        if let Some(value) = hh_value(text, "RESOLUTION")
            .and_then(|value| value.split_whitespace().find(|part| part.contains("/MM")))
            .and_then(parse_f64)
        {
            s.identity.resolution_dpi =
                Observed::value((value * 25.4).round() as u64, Some("dpi"), "^HH");
        }
    }
    if let Some(value) = hh_value(text, "ONBOARD FLASH").and_then(parse_u64) {
        s.memory.flash_total_bytes = Observed::value(value * 1024, Some("bytes"), "^HH");
    }
    if let Some(value) = hh_value(text, "TOTAL USAGE").and_then(parse_f64) {
        s.counters.odometer_meters = Observed::value(value * 0.0254, Some("meters"), "^HH");
    }
    if let Some(value) = hh_value(text, "LAST CLEANED").and_then(parse_f64) {
        s.maintenance.last_cleaned_meters = Observed::value(value * 0.0254, Some("meters"), "^HH");
    }
    if let Some(value) = hh_value(text, "HEAD USAGE").and_then(parse_f64) {
        s.maintenance.head_usage_meters = Observed::value(value * 0.0254, Some("meters"), "^HH");
    }
    Ok(())
}

fn named_number(text: &str, label: &str) -> Option<u64> {
    lines(text).find_map(|line| {
        let value = line
            .strip_prefix(label)?
            .trim()
            .trim_start_matches(':')
            .trim();
        parse_u64(value)
    })
}

fn parse_hq_errors(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let errors = named_number(text, "ERRORS").ok_or("missing_error_count")?;
    let warnings = named_number(text, "WARNINGS").unwrap_or(0);
    s.capabilities.insert(
        "active_errors".into(),
        Observed::value(errors > 0, None, "~HQES"),
    );
    s.capabilities.insert(
        "active_warnings".into(),
        Observed::value(warnings > 0, None, "~HQES"),
    );
    Ok(())
}

fn parse_hq_head_test(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let result = lines(text)
        .find(|line| line.contains(','))
        .ok_or("missing_head_test_result")?;
    s.diagnostics.head_test = Observed::value(result.to_string(), None, "~HQJT");
    Ok(())
}

fn parse_hq_maintenance(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let interval =
        named_number(text, "HEAD REPLACEMENT INTERVAL").ok_or("missing_maintenance_interval")?;
    s.maintenance.head_replacement_interval_meters =
        Observed::value(interval as f64 * 1000.0, Some("meters"), "~HQMA");
    if let Some(value) = lines(text).find_map(|line| line.strip_prefix("PRINT REPLACEMENT ALERT:"))
    {
        set_bool(
            &mut s.maintenance.replacement_alert,
            bool01(Some(&value.trim())),
            "~HQMA",
        );
    }
    if let Some(value) = lines(text).find_map(|line| line.strip_prefix("PRINT CLEANING ALERT:")) {
        set_bool(
            &mut s.maintenance.cleaning_alert,
            bool01(Some(&value.trim())),
            "~HQMA",
        );
    }
    Ok(())
}

fn parse_hq_odometer(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let total = named_number(text, "TOTAL NONRESETTABLE").ok_or("missing_total_odometer")?;
    s.counters.odometer = Observed::value(total, Some("inches"), "~HQOD");
    s.counters.odometer_meters = Observed::value(total as f64 * 0.0254, Some("meters"), "~HQOD");
    if let Some(value) = named_number(text, "USER RESETTABLE CNTR1") {
        s.counters.resettable_counter_1_meters =
            Observed::value(value as f64 * 0.0254, Some("meters"), "~HQOD");
    }
    if let Some(value) = named_number(text, "USER RESETTABLE CNTR2") {
        s.counters.resettable_counter_2_meters =
            Observed::value(value as f64 * 0.0254, Some("meters"), "~HQOD");
    }
    Ok(())
}

fn parse_hq_head_life(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let value = named_number(text, "LAST CLEANED").ok_or("missing_last_cleaned")?;
    s.maintenance.last_cleaned_meters =
        Observed::value(value as f64 * 0.0254, Some("meters"), "~HQPH");
    Ok(())
}

fn colon_value<'a>(text: &'a str, label: &str) -> Option<&'a str> {
    lines(text).find_map(|line| line.strip_prefix(label).map(str::trim))
}

fn parse_hq_plug_and_play(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let model = colon_value(text, "MDL:").ok_or("missing_model")?;
    s.identity.model = Observed::value(model.to_string(), None, "~HQPP");
    if let Some(value) = colon_value(text, "MFG:") {
        s.identity.manufacturer = Observed::value(value.to_string(), None, "~HQPP");
    }
    if let Some(value) = colon_value(text, "CMD:") {
        s.identity.command_language = Observed::value(value.to_string(), None, "~HQPP");
    }
    Ok(())
}

fn parse_hq_serial(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let (_, response) = text
        .rsplit_once("SERIAL NUMBER")
        .ok_or("missing_serial_header")?;
    let serial = lines(response).next().ok_or("missing_serial")?;
    s.identity.serial_number = Observed::value(serial.to_string(), None, "~HQSN");
    Ok(())
}

fn parse_hq_usb(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let pid = colon_value(text, "PID:").ok_or("missing_usb_pid")?;
    s.interfaces.usb_product_id = Observed::value(pid.to_string(), None, "~HQUI");
    if let Some(value) = colon_value(text, "RELEASE VERSION:") {
        s.interfaces.usb_release_version = Observed::value(value.to_string(), None, "~HQUI");
    }
    Ok(())
}

fn xml_value<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let start = text.find(&open)?;
    let content = text[start..].find('>')? + start + 1;
    let close = format!("</{tag}>");
    let end = text[content..].find(&close)? + content;
    Some(text[content..end].trim())
}

fn xml_bool(text: &str, tag: &str) -> Option<bool> {
    match xml_value(text, tag)? {
        "Y" => Some(true),
        "N" => Some(false),
        _ => None,
    }
}

fn xml_u64(text: &str, tag: &str) -> Option<u64> {
    xml_value(text, tag).and_then(parse_u64)
}

fn parse_xml_status(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    if !text.contains("<STATUS>") {
        return Err("missing_xml_status".into());
    }
    for (tag, target) in [
        ("PAUSE", &mut s.status.paused),
        ("PAPER-OUT", &mut s.status.media_out),
        ("RIBBON-OUT", &mut s.status.ribbon_out),
        ("HEAD-OPEN", &mut s.status.head_open),
        ("BUFFER-FULL-ERROR", &mut s.status.buffer_full),
        ("CUTTER-JAM-ERROR", &mut s.status.cutter_jam),
        ("COVER-OPEN", &mut s.status.cover_open),
        ("CLEAN-PRINTHEAD-WARNING", &mut s.status.clean_head_warning),
        ("MEDIA-LOW-WARNING", &mut s.status.media_low),
        ("RIBBON-LOW-WARNING", &mut s.status.ribbon_low),
    ] {
        set_bool(target, xml_bool(text, tag), "^HZr");
    }
    if let Some(value) = xml_bool(text, "HEAD-ELEMENT-STATUS") {
        s.diagnostics.head_element_failed = Observed::value(value, None, "^HZr");
    } else if let Some(value) = xml_bool(text, "FAILED") {
        s.diagnostics.head_element_failed = Observed::value(value, None, "^HZr");
    }
    let under = xml_bool(text, "HEAD-UNDERTEMP-WARNING").unwrap_or(false);
    let over = xml_bool(text, "HEAD-OVERTEMP-ERROR").unwrap_or(false);
    s.status.temperature_fault = Observed::value(under || over, None, "^HZr");
    if let Some(block) = xml_value(text, "PRINTHEAD-TEMP") {
        if let Some(value) = xml_value(block, "CURRENT").and_then(parse_f64) {
            s.diagnostics.temperature_celsius = Observed::value(value, Some("celsius"), "^HZr");
        }
        if let Some(value) = xml_value(block, "OVERTEMP-THRESHOLD").and_then(parse_f64) {
            s.diagnostics.overtemp_threshold_celsius =
                Observed::value(value, Some("celsius"), "^HZr");
        }
        if let Some(value) = xml_value(block, "UNDERTEMP-THRESHOLD").and_then(parse_f64) {
            s.diagnostics.undertemp_threshold_celsius =
                Observed::value(value, Some("celsius"), "^HZr");
        }
    }
    if let Some(value) = xml_u64(text, "TOTAL-LABELS-IN-BATCH") {
        s.status.batch_total = Observed::value(value, Some("labels"), "^HZr");
    }
    if let Some(value) = xml_u64(text, "LABELS-REMAINING-IN-BATCH") {
        s.status.batch_remaining = Observed::value(value, Some("labels"), "^HZr");
    }
    if let Some(value) = xml_u64(text, "NUMBER-OF-FORMATS") {
        s.status.formats_buffered = Observed::value(value, Some("formats"), "^HZr");
    }
    if let Some(value) = xml_u64(text, "MEDIA-REPLACED") {
        s.maintenance.media_replaced = Observed::value(value, Some("events"), "^HZr");
    }
    if let Some(value) = xml_u64(text, "RIBBON-REPLACED") {
        s.maintenance.ribbon_replaced = Observed::value(value, Some("events"), "^HZr");
    }
    if let Some(value) = xml_u64(text, "HEAD-CLEANED") {
        s.maintenance.head_cleaned = Observed::value(value, Some("events"), "^HZr");
    }
    if let Some(block) = xml_value(text, "NON-RESET-COUNTER") {
        if let Some(value) = xml_u64(block, "CENTIMETERS") {
            s.counters.odometer_meters =
                Observed::value(value as f64 / 100.0, Some("meters"), "^HZr");
        }
        if let Some(value) = xml_u64(block, "LABELS") {
            s.counters.labels_printed = Observed::value(value, Some("labels"), "^HZr");
        }
    }
    let fault = [
        &s.status.paused,
        &s.status.media_out,
        &s.status.ribbon_out,
        &s.status.head_open,
        &s.status.temperature_fault,
        &s.status.buffer_full,
        &s.status.cutter_jam,
    ]
    .iter()
    .any(|v| v.value == Some(true));
    s.status.ready = Observed::value(!fault, None, "^HZr");
    Ok(())
}

fn parse_sgd_odometer(text: &str, s: &mut PrinterSnapshot) -> Result<(), String> {
    let response = lines(text)
        .find(|line| line.to_ascii_uppercase().contains("INCHES"))
        .ok_or("missing_sgd_odometer")?
        .trim_matches('"');
    let inches = response
        .split(',')
        .next()
        .and_then(parse_u64)
        .ok_or("missing_sgd_odometer")?;
    s.counters.odometer = Observed::value(inches, Some("inches"), "SGD");
    s.counters.odometer_meters = Observed::value(inches as f64 * 0.0254, Some("meters"), "SGD");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lp2824_status_identification_memory_and_diagnostics() {
        let mut s = PrinterSnapshot::new("ente".into());
        parse(
            "host_status",
            b"\x02030,0,0,0251,000,0,0,0,000,0,0,0\x03\r\n\x02000,0,0,0,0,2,3,0,00000000,1,000\x03\r\n\x021234,0\x03\r\n",
            &mut s,
        )
        .unwrap();
        parse(
            "identification",
            b"\x02LP 2824 Plus-200dpi,V61.17.5Z,8,2104KB\x03\r\n",
            &mut s,
        )
        .unwrap();
        parse("memory", b"\x022104,2006,2006\x03\r\n", &mut s).unwrap();
        parse(
            "head_diagnostics",
            b"\x02Head Temp = 28\r\nHead Test = Passed\r\nDarkness Adjust = 22.6\r\nPrint Speed = 3\r\nSlew Speed = 3\r\nBackfeed Speed = 2\r\nDynamic_top_position = -008\r\n\x03",
            &mut s,
        )
        .unwrap();
        assert_eq!(s.identity.model.value.as_deref(), Some("LP 2824 Plus"));
        assert_eq!(s.identity.resolution_dpi.value, Some(200));
        assert_eq!(s.memory.ram_free_bytes.value, Some(2_054_144));
        assert_eq!(s.diagnostics.temperature_celsius.value, Some(28.0));
        assert_eq!(s.settings.label_top_dots.value, Some(-8));
        assert_eq!(s.status.ready.value, Some(true));
    }

    #[test]
    fn finds_typed_responses_after_a_delayed_host_status() {
        let mut s = PrinterSnapshot::new("ente".into());
        let delayed = b"\x02030,0,0,0251,000,0,0,0,000,0,0,0\x03\r\n\x02000,0,0,0,0,2,3,0,00000000,1,000\x03\r\n\x021234,0\x03\r\n";
        let mut identification = delayed.to_vec();
        identification.extend_from_slice(b"\x02LP 2824 Plus-200dpi,V61.17.5Z,8,2104KB\x03\r\n");
        parse("identification", &identification, &mut s).unwrap();
        let mut memory = identification;
        memory.extend_from_slice(b"\x022104,2006,2006\x03\r\n");
        parse("memory", &memory, &mut s).unwrap();
        assert_eq!(s.identity.model.value.as_deref(), Some("LP 2824 Plus"));
        assert_eq!(s.identity.firmware.value.as_deref(), Some("V61.17.5Z"));
        assert_eq!(s.memory.ram_total_bytes.value, Some(2_154_496));
    }

    #[test]
    fn parses_lp2824_xml_status() {
        let mut s = PrinterSnapshot::new("ente".into());
        let xml = "<STATUS><TOTAL-LABELS-IN-BATCH>4</TOTAL-LABELS-IN-BATCH><LABELS-REMAINING-IN-BATCH>3</LABELS-REMAINING-IN-BATCH><PRINTHEAD-TEMP><OVERTEMP-THRESHOLD>300</OVERTEMP-THRESHOLD><UNDERTEMP-THRESHOLD>-30</UNDERTEMP-THRESHOLD><CURRENT>29</CURRENT></PRINTHEAD-TEMP><FAILED BOOL='Y,N'>N</FAILED><HEAD-OPEN BOOL='Y,N'>N</HEAD-OPEN><HEAD-UNDERTEMP-WARNING BOOL='Y,N'>N</HEAD-UNDERTEMP-WARNING><HEAD-OVERTEMP-ERROR BOOL='Y,N'>N</HEAD-OVERTEMP-ERROR><BUFFER-FULL-ERROR BOOL='Y,N'>N</BUFFER-FULL-ERROR><CUTTER-JAM-ERROR BOOL='Y,N'>N</CUTTER-JAM-ERROR><COVER-OPEN BOOL='Y,N'>N</COVER-OPEN><CLEAN-PRINTHEAD-WARNING BOOL='Y,N'>N</CLEAN-PRINTHEAD-WARNING><MEDIA-LOW-WARNING BOOL='Y,N'>N</MEDIA-LOW-WARNING><RIBBON-LOW-WARNING BOOL='Y,N'>N</RIBBON-LOW-WARNING><PAUSE BOOL='Y,N'>N</PAUSE><PAPER-OUT BOOL='Y,N'>N</PAPER-OUT><RIBBON-OUT BOOL='Y,N'>N</RIBBON-OUT><NUMBER-OF-FORMATS>1</NUMBER-OF-FORMATS><PRINTER-COUNTER><NON-RESET-COUNTER><INCHES>190609</INCHES><CENTIMETERS>484146</CENTIMETERS><LABELS>7</LABELS></NON-RESET-COUNTER></PRINTER-COUNTER><MEDIA-REPLACED>9</MEDIA-REPLACED><RIBBON-REPLACED>0</RIBBON-REPLACED><HEAD-CLEANED>2</HEAD-CLEANED></STATUS>";
        parse("xml_status", xml.as_bytes(), &mut s).unwrap();
        assert_eq!(s.status.batch_remaining.value, Some(3));
        assert_eq!(s.status.ready.value, Some(true));
        assert_eq!(s.counters.odometer_meters.value, Some(4841.46));
        assert_eq!(s.counters.labels_printed.value, Some(7));
        assert_eq!(s.maintenance.media_replaced.value, Some(9));
    }
}
