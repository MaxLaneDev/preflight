"""Exercise the shell installer offline; no release download or actual installation."""
import hashlib,json,os,pathlib,subprocess,tempfile,unittest
ROOT=pathlib.Path(__file__).resolve().parents[1]
class Installer(unittest.TestCase):
 def run_installer(self,arch='arm64',version='latest',corrupt=False):
  with tempfile.TemporaryDirectory() as temporary:
   root=pathlib.Path(temporary);bin=root/'bin';bin.mkdir();assets=root/'assets';assets.mkdir()
   installer=b'import json,sys;print(json.dumps(sys.argv[1:]))\n'
   (assets/'install-preflight.py').write_bytes(installer)
   archive='preflight-0.1.0-macos-aarch64.tar.gz'
   (assets/'SHA256SUMS').write_text(('0'*64 if corrupt else hashlib.sha256(installer).hexdigest())+'  install-preflight.py\n'+'a'*64+'  '+archive+'\n')
   (bin/'uname').write_text('#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo '+arch+';; esac\n')
   (bin/'curl').write_text('#!/usr/bin/env python3\nimport os,sys,pathlib,shutil\na=sys.argv[1:];url=next(v for v in a if v.startswith("https://"));target=a[a.index("-o")+1];shutil.copyfile(pathlib.Path(os.environ["ASSETS"])/url.rsplit("/",1)[1],target)\n')
   for p in bin.iterdir():p.chmod(0o700)
   return subprocess.run(['sh',str(ROOT/'install.sh')],env={**os.environ,'PATH':str(bin)+':'+os.environ['PATH'],'ASSETS':str(assets),'PREFLIGHT_VERSION':version,'PREFLIGHT_PREFIX':str(root/'prefix with spaces'),'PREFLIGHT_CONFIG':str(root/'config with spaces.toml')},text=True,capture_output=True)
 def test_verified_install_and_quoted_paths(self):
  result=self.run_installer();self.assertEqual(result.returncode,0,result.stderr);args=json.loads(result.stdout);self.assertEqual(args[0],'install');self.assertTrue(args[1].startswith('https://github.com/MaxLaneDev/preflight/releases/latest/download/'));self.assertTrue(args[args.index('--prefix')+1].endswith('prefix with spaces'))
 def test_corruption_never_executes_installer(self):
  result=self.run_installer(corrupt=True);self.assertNotEqual(result.returncode,0);self.assertIn('checksum mismatch',result.stderr);self.assertFalse(result.stdout)
 def test_deferred_arch_and_invalid_versions_fail_before_download(self):
  for kwargs in [{'arch':'x86_64'},{'version':'v1;touch bad'},{'version':'https://elsewhere'}]:
   result=self.run_installer(**kwargs);self.assertNotEqual(result.returncode,0);self.assertFalse(result.stdout)
if __name__=='__main__':unittest.main()
