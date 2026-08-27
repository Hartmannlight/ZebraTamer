"""Create immutable binary releases; main builds are never promoted to stable."""
import json
import os
import re
import subprocess
from pathlib import Path

sha = os.environ['GITHUB_SHA']
ref = os.environ['GITHUB_REF']
stable = ref.startswith('refs/tags/')
tag = ref.removeprefix('refs/tags/') if stable else f'build-{sha}-r{os.environ["GITHUB_RUN_ID"]}-{os.environ["GITHUB_RUN_ATTEMPT"]}'
if stable and not re.fullmatch(r'v\d+\.\d+\.\d+', tag):
    raise SystemExit('Only exact vMAJOR.MINOR.PATCH releases are supported')
subprocess.run(['git', 'fetch', 'origin', 'main'], check=True)
subprocess.run(['git', 'merge-base', '--is-ancestor', sha, 'FETCH_HEAD'], check=True)
result = subprocess.run(['gh', 'api', f'repos/{os.environ["GITHUB_REPOSITORY"]}/releases/tags/{tag}'], capture_output=True, text=True)
if result.returncode == 0:
    raise SystemExit('Refusing to overwrite an existing release')
if '404' not in result.stderr:
    raise SystemExit('Cannot verify whether release exists: ' + result.stderr)
Path('release/VERSION').write_text(tag + '\n')
files = [str(path) for path in sorted(Path('release').iterdir()) if path.is_file()]
command = ['gh', 'release', 'create', tag, '--target', sha, '--title', tag, '--notes',
           'Validated Linux executables for AMD64, ARM64 and ARMv7. Checksums, source commit, dependency SBOM and GitHub provenance are included. Installation and upgrades remain an operator action.', *files]
if not stable:
    command += ['--prerelease', '--latest=false']
else:
    command += ['--verify-tag']
subprocess.run(command, check=True)
with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as summary:
    summary.write(f'Binary release `{tag}` from `{sha}` published. No container image or deployment.\n')
