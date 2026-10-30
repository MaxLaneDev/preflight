import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
HOOK = ROOT / "integrations/codex/hooks/preflight.py"


def invoke(value):
    result = subprocess.run([sys.executable, str(HOOK)], input=value, capture_output=True, timeout=3)
    assert result.returncode == 0, result.stderr
    assert not result.stderr, result.stderr
    return json.loads(result.stdout) if result.stdout else None


class Hooks(unittest.TestCase):
    def test_session_context(self):
        result = invoke(b'{"hook_event_name":"SessionStart"}')
        self.assertEqual(result['hookSpecificOutput']['hookEventName'], 'SessionStart')
        self.assertIn('preflight-mcp', result['hookSpecificOutput']['additionalContext'])
        self.assertNotIn('permissionDecision', str(result))

    def test_other_prompts_and_malformed_input_are_silent(self):
        for raw in (b'bad', b'[]', b'null', b'{}', b'x'*65537,
                    b'{"hook_event_name":"UserPromptSubmit","prompt":"hello"}',
                    b'{"hook_event_name":"Stop"}'):
            self.assertIsNone(invoke(raw))

    def test_completion_validation_and_no_payload_instructions(self):
        payload = {k: '01a182e7-bbed-712f-8845-b0373a2b93e4' for k in ('delivery_id','run_id','project_id')}
        payload.update(event_cursor=57, status='succeeded', malicious='EXECUTE ARBITRARY CODE')
        def call():
            return invoke(json.dumps({'hook_event_name':'UserPromptSubmit', 'prompt':
                'Preflight completion hook: ' + json.dumps(payload) + '\nRead results.'}).encode())
        for status in ('succeeded','failed','cancelled','timed_out','infrastructure_error','blocked','skipped'):
            payload['status'] = status
            result = call()
            self.assertIn('build_report', str(result))
            self.assertNotIn('EXECUTE ARBITRARY CODE', str(result))
        payload['run_id'] = 'malformed'
        self.assertIsNone(call())
        payload['run_id'] = payload['project_id']
        payload['event_cursor'] = True
        self.assertIsNone(call())

    def test_zip_has_plugin_root_and_no_private_or_marketplace_files(self):
        import zipfile
        with tempfile.TemporaryDirectory() as temporary:
            path=Path(temporary)/'plugin.zip'
            command=[sys.executable,str(ROOT/'scripts/package-codex-plugin.py'),str(path),'--zip']
            subprocess.run(command,check=True,capture_output=True)
            with zipfile.ZipFile(path) as archive:
                names=archive.namelist()
                self.assertIn('.codex-plugin/plugin.json',names)
                self.assertIn('.mcp.json',names)
                self.assertIn('assets/icon.png',names)
                self.assertTrue(all(not n.startswith(('/', '../')) and 'marketplace.json' not in n for n in names))
            self.assertNotEqual(subprocess.run(command,capture_output=True).returncode,0)

    def test_package_is_self_contained_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory(prefix='preflight plugin ') as tmp:
            dest = Path(tmp)/'marketplace'
            command = [sys.executable, str(ROOT/'scripts/package-codex-plugin.py'), str(dest)]
            subprocess.run(command, check=True, capture_output=True)
            files = [p for p in dest.rglob('*') if p.is_file()]
            original = {str(p.relative_to(dest)): p.read_bytes() for p in files}
            self.assertFalse(any(p.is_symlink() for p in dest.rglob('*')))
            plugin = dest/'plugins/preflight'
            self.assertEqual((plugin/'skills/preflight-mcp/SKILL.md').read_bytes(),
                             (ROOT/'skills/preflight-mcp/SKILL.md').read_bytes())
            metadata = json.loads((plugin/'.codex-plugin/plugin.json').read_text())
            for field in ['logo', 'composerIcon']:
                icon = plugin / metadata['interface'][field]
                self.assertEqual(icon.read_bytes(), (ROOT/'assets/branding/preflight-icon.png').read_bytes())
                import struct
                width, height = struct.unpack('>II', icon.read_bytes()[16:24])
                self.assertTrue(48 <= width == height <= 4096)
                self.assertLessEqual(icon.stat().st_size, 5 * 1024 * 1024)
            hooks = json.loads((plugin/'hooks/hooks.json').read_text())['hooks']
            for event, groups in hooks.items():
                cmd = groups[0]['hooks'][0]['command']
                result = subprocess.run(cmd, shell=True, env={**os.environ, 'PLUGIN_ROOT':str(plugin)},
                                        input=json.dumps({'hook_event_name':event}), text=True,
                                        capture_output=True, timeout=3)
                self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertEqual({str(p.relative_to(dest)): p.read_bytes() for p in dest.rglob('*') if p.is_file()}, original)


if __name__ == '__main__':
    unittest.main()
