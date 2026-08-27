"""Fail closed on fixable High/Critical findings; export reports without secret values."""
import json
import sys
from pathlib import Path

def findings(report):
    if not isinstance(report, dict) or 'Results' not in report:
        raise ValueError('Missing Trivy Results: scanner output must not be empty')
    blocked, unpatched = [], []
    for result in report['Results'] or []:
        for finding in result.get('Vulnerabilities', []) or []:
            if finding.get('Severity') in ('HIGH', 'CRITICAL'):
                item = (finding.get('VulnerabilityID'), finding.get('PkgName'))
                (blocked if finding.get('FixedVersion') else unpatched).append(item)
        for kind in ('Secrets', 'Misconfigurations'):
            for finding in result.get(kind, []) or []:
                if finding.get('Severity') in ('HIGH', 'CRITICAL') and finding.get('Status', 'FAIL') != 'PASS':
                    blocked.append((finding.get('ID', finding.get('RuleID', kind)), result.get('Target')))
    return blocked, unpatched

def self_test():
    base = {'Results': [{'Vulnerabilities': [{'VulnerabilityID': 'CVE-TEST', 'PkgName': 'fixture', 'Severity': 'HIGH', 'FixedVersion': '2'}]}]}
    assert findings(base)[0]
    base['Results'][0]['Vulnerabilities'][0]['FixedVersion'] = ''
    assert findings(base)[1] and not findings(base)[0]
    assert findings({'Results': [{'Secrets': [{'Severity': 'CRITICAL', 'RuleID': 'fixture'}]}]})[0]
    try:
        findings({})
    except ValueError:
        pass
    else:
        raise AssertionError('Incomplete report accepted')
    print('Scan policy self-test passed')

if __name__ == '__main__':
    if sys.argv[1:] == ['--self-test']:
        self_test()
    else:
        blocked, unpatched = [], []
        for name in sys.argv[1:]:
            report = json.loads(Path(name).read_text())
            fail, pending = findings(report)
            for result in report['Results'] or []:
                for secret in result.get('Secrets', []) or []:
                    for key in ('Match', 'Code'):
                        secret.pop(key, None)
            Path('artifacts').mkdir(exist_ok=True)
            Path('artifacts', Path(name).name).write_text(json.dumps(report, indent=2))
            blocked.extend(fail)
            unpatched.extend(pending)
        if not sys.argv[1:]:
            raise SystemExit('At least one scan report is required')
        print(json.dumps({'blocked': blocked, 'unpatched_requires_triage': unpatched}, indent=2))
        raise SystemExit(1 if blocked else 0)
