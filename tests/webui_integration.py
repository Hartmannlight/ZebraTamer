"""Linux HTTP/PTY integration tests, never physical hardware.

python3 tests/webui_integration.py target/debug/zpl-agent [--serve]
"""
import json
import os
from pathlib import Path
import pty
import re
import select
import socket
import subprocess
import sys
import tempfile
import threading
import time
import tty
import unittest
import urllib.error
import urllib.request

TOKEN = 'integration-test-token-not-for-production'
BINARY = str(Path(sys.argv.pop(1)).resolve())
BASE = '/v1/printers/test'
MEDIA = {'display_name':'Lageretiketten gelb', 'width_mm':50, 'height_mm':25,
         'shape':'rectangle', 'tracking':'gap', 'print_technology':'direct_thermal',
         'color':{'name':'Gelb','hex':'#f1e6a3'}, 'preferred_settings':{},
         'labels_available_at_load':500, 'low_warning_threshold':20}


class Printer:
    def __init__(self):
        self.master, self.slave = pty.openpty()
        tty.setraw(self.slave)
        self.path = os.ttyname(self.slave)
        self.values = {'~SD':'10.0', '^PR':'3', '^LS':'0', '^LT':'0', '^PW':'400', '^LL':'200', '^MM':'T', '^MT':'D', '^MN':'Y'}
        self.saved = dict(self.values)
        self.commands = []
        self.ignore = False
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def report(self):
        v = self.values
        mode = {'T':'TEAR OFF', 'P':'PEEL OFF', 'C':'CUTTER'}[v['^MM']]
        method = {'D':'DIRECT-THERMAL', 'T':'THERMAL-TRANS'}[v['^MT']]
        tracking = 'CONTINUOUS' if v['^MN'] == 'N' else 'NON-CONTINUOUS'
        sensor = 'MARK' if v['^MN'] == 'M' else 'WEB'
        return (f"Zebra PTY FIRMWARE\n{v['~SD']} DARKNESS\n{v['^PR']} IPS PRINT SPEED\n"
                f"{v['^LS']} LEFT POSITION\n{v['^LT']} LABEL TOP\n{v['^PW']} PRINT WIDTH\n"
                f"{v['^LL']} LABEL LENGTH\n{mode} PRINT MODE\n{method} PRINT METHOD\n"
                f"{tracking} MEDIA TYPE\n{sensor} SENSOR TYPE\n8/MM FULL RESOLUTION\n").encode()

    def run(self):
        while not self.stop.is_set():
            if not select.select([self.master], [], [], .1)[0]:
                continue
            try:
                data = os.read(self.master, 65536).decode('utf-8', errors='replace')
            except OSError:
                continue
            for command, value in re.findall(r'([\^~][A-Z]{2})([^\^~\r\n]*)', data):
                self.commands.append(command + value)
                if command in self.values and not self.ignore:
                    self.values[command] = value
                if command == '^JU' and value == 'S':
                    self.saved = dict(self.values)
                if command == '^HH':
                    os.write(self.master, self.report())
                elif command == '~HS':
                    os.write(self.master, b'000,0,0,0,0,0,0\n')
                elif command in ('~HD','~HM','~HB','^HW','~HQ'):
                    os.write(self.master, b'0\n')
            if '! U1' in data:
                os.write(self.master, b'unsupported\n')

    def close(self):
        self.stop.set()
        self.thread.join(timeout=2)
        os.close(self.master)
        os.close(self.slave)


class Agent:
    def __init__(self, port=0):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.printer = Printer()
        if not port:
            with socket.socket() as listener:
                listener.bind(('127.0.0.1', 0))
                port = listener.getsockname()[1]
        self.url = f'http://127.0.0.1:{port}'
        self.config = self.root / 'config.toml'
        self.config.write_text(f'''listen = "0.0.0.0:{port}"
data_dir = "{self.root / 'data'}"
mdns_enabled = false
webui_enabled = true
admin_token = "{TOKEN}"
poll_interval_secs = 3600
capability_poll_interval_secs = 3600
host_boot_loss_labels = 0
printer_reconnect_loss_labels = 0
[[printers]]
id = "test"
display_name = "Zebra · Testgerät"
device = "{self.printer.path}"
first_byte_timeout_ms = 100
idle_timeout_ms = 10
[printers.device_profile]
resolution_dpi = 203
peel_off = true
thermal_transfer = true
''', encoding='utf-8')
        self.log = (self.root / 'agent.log').open('w+')
        self.start()

    def start(self):
        self.process = subprocess.Popen([BINARY, '--config', str(self.config)], stdout=self.log, stderr=self.log)
        for _ in range(100):
            try:
                self.request('/healthz')
                return
            except OSError:
                if self.process.poll() is not None:
                    self.log.seek(0)
                    raise RuntimeError(self.log.read())
                time.sleep(.05)
        raise RuntimeError('agent startup timed out')

    def request(self, path, method='GET', body=None, token=TOKEN):
        headers = {'Accept':'application/json'}
        if token:
            headers['Authorization'] = f'Bearer {token}'
        if body is not None:
            headers['Content-Type'] = 'application/json'
        request = urllib.request.Request(self.url + path, method=method, headers=headers,
                                         data=None if body is None else json.dumps(body).encode())
        try:
            response = urllib.request.urlopen(request, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            data = response.read().decode()
            try:
                data = json.loads(data)
            except ValueError:
                pass
            return response.status, data

    def stop(self):
        self.process.terminate()
        self.process.wait(timeout=5)

    def close(self):
        self.stop()
        self.printer.close()
        self.log.close()
        self.temp.cleanup()


class IntegrationTests(unittest.TestCase):
    def setUp(self):
        self.agent = Agent()
        self.addCleanup(self.agent.close)

    def test_optional_ui_and_admin_token(self):
        status, html = self.agent.request('/ui/')
        self.assertEqual(status, 200)
        self.assertIn('Drucker & Material', html)
        self.assertNotIn(TOKEN, html)
        self.assertEqual(self.agent.request(BASE + '/configuration/read', 'POST', token=None)[0], 401)
        self.assertEqual(self.agent.request(BASE + '/media', 'PUT', MEDIA, token=None)[0], 401)
        self.agent.stop()
        self.agent.config.write_text(self.agent.config.read_text().replace('webui_enabled = true', 'webui_enabled = false'))
        self.agent.start()
        self.assertEqual(self.agent.request('/ui/')[0], 404)
        self.assertEqual(self.agent.request('/v1/printers')[0], 200)

    def test_save_and_stale_revision(self):
        status, result = self.agent.request(BASE + '/configuration/read', 'POST')
        self.assertEqual(status, 200, result)
        request = {'revision':result['data']['revision'], 'settings':{'darkness':12, 'x_offset':2, 'print_mode':'peel_off', 'print_method':'thermal_transfer'}, 'confirm_save_all':True}
        status, result = self.agent.request(BASE + '/configuration', 'POST', request)
        self.assertEqual(status, 200, result)
        self.assertEqual(result['data']['state'], 'save_sent_active_verified', result)
        self.assertEqual(self.agent.printer.saved['~SD'], '12')
        self.assertEqual(self.agent.printer.saved['^LS'], '2')
        saves = self.agent.printer.commands.count('^JUS')
        self.assertEqual(self.agent.request(BASE + '/configuration', 'POST', request)[0], 409)
        self.assertEqual(self.agent.printer.commands.count('^JUS'), saves)

    def test_rejected_settings_are_not_persisted(self):
        _, result = self.agent.request(BASE + '/configuration/read', 'POST')
        self.agent.printer.ignore = True
        status, result = self.agent.request(BASE + '/configuration', 'POST', {'revision':result['data']['revision'], 'settings':{'darkness':13}, 'confirm_save_all':True})
        self.assertEqual(status, 200, result)
        self.assertEqual(result['data']['state'], 'not_saved')
        self.assertNotIn('^JUS', self.agent.printer.commands)

    def test_invalid_or_oversized_settings_are_rejected_before_device_writes(self):
        body = {'revision':'unused', 'settings':{'print_mode':'tear_off^JUF'}, 'confirm_save_all':True}
        self.assertEqual(self.agent.request(BASE + '/configuration', 'POST', body)[0], 422)
        body = {'revision':'x' * 70000, 'settings':{'darkness':12}, 'confirm_save_all':True}
        self.assertEqual(self.agent.request(BASE + '/configuration', 'POST', body)[0], 413)
        self.assertNotIn('^JUS', self.agent.printer.commands)

    def test_media_color_and_counter_survive_restart(self):
        self.assertEqual(self.agent.request(BASE + '/media', 'PUT', MEDIA)[0], 200)
        _, config = self.agent.request(BASE + '/configuration')
        revision = config['data']['media']['revision']
        self.agent.request(BASE + '/media/adjustments', 'POST', {'delta':-7,'reason':'test','source':'test'})
        changed = {**MEDIA, 'color':{'name':'Blau','hex':'#4488cc'}}
        self.assertEqual(self.agent.request(BASE + '/media', 'PATCH', {'revision':revision, 'media':changed})[0], 200)
        self.agent.stop()
        self.agent.start()
        _, media = self.agent.request(BASE + '/media')
        self.assertEqual(media['data']['value']['media']['color']['name'], 'Blau')
        self.assertEqual(media['data']['value']['remaining_labels'], 493)
        self.assertNotIn('^JUS', self.agent.printer.commands)


if __name__ == '__main__':
    if '--serve' in sys.argv:
        agent = Agent(port=8080)
        agent.request(BASE + '/media', 'PUT', MEDIA)
        agent.request(BASE + '/configuration/read', 'POST')
        print(f'Browser fixture ready at :8080/ui/; test token: {TOKEN}', flush=True)
        try:
            threading.Event().wait()
        except KeyboardInterrupt:
            pass
        finally:
            agent.close()
    else:
        unittest.main()
