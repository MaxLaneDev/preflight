"""Package trust-boundary and activation regression checks; owns all temporary files."""
import hashlib,importlib.util,io,json,pathlib,platform,sys,tarfile,tempfile,unittest
spec=importlib.util.spec_from_file_location('release',pathlib.Path(__file__).with_name('release.py'));release=importlib.util.module_from_spec(spec);spec.loader.exec_module(release)
class ReleaseTests(unittest.TestCase):
 def setUp(self):self.temp=tempfile.TemporaryDirectory();self.root=pathlib.Path(self.temp.name)
 def tearDown(self):self.temp.cleanup()
 def archive(self,content=b'first',name='preflight',extra=None,platform_info=None):
  host_os={'darwin':'macos','win32':'windows'}.get(sys.platform,sys.platform);host_arch={'arm64':'aarch64','AMD64':'x86_64'}.get(platform.machine(),platform.machine())
  if platform_info:host_os,host_arch=platform_info
  manifest={'format':'preflight-package-v1','component':'preflight','binary':name,'metadata':{'os':host_os,'arch':host_arch,'version':'0.1.0'},'files':{name:{'sha256':hashlib.sha256(content).hexdigest(),'size':len(content),'executable':True}}}
  path=self.root/(hashlib.sha256(content+name.encode()).hexdigest()+'.tar.gz')
  with tarfile.open(path,'w:gz') as tar:
   data=json.dumps(manifest).encode();header=tarfile.TarInfo('manifest.json');header.size=len(data);tar.addfile(header,io.BytesIO(data))
   header=tarfile.TarInfo(name);header.size=len(content);tar.addfile(header,io.BytesIO(content))
   if extra:tar.addfile(extra)
  return path,release.digest(path)
 def test_checksum_paths_links_and_duplicates_fail_before_activation(self):
  path,sha=self.archive()
  with self.assertRaises(ValueError):release.verify(path,'0'*64)
  for name in ['../escape','/absolute','a/../escape','windows\\escape','C:/escape','file:stream','NUL','trailing.']:
   path,sha=self.archive(name=name)
   with self.assertRaises(ValueError):release.verify(path,sha)
  link=tarfile.TarInfo('link');link.type=tarfile.SYMTYPE;link.linkname='/outside'
  path,sha=self.archive(extra=link)
  with self.assertRaises(ValueError):release.verify(path,sha)
  path,sha=self.archive(extra=tarfile.TarInfo('manifest.json'))
  with self.assertRaises(ValueError):release.verify(path,sha)
 def test_install_update_replay_and_rollback_preserve_data(self):
  first,sha1=self.archive();second,sha2=self.archive(b'second');destination=self.root/'installation'
  release.install(first,sha1,destination);release.install(first,sha1,destination)
  data=self.root/'server-data';data.mkdir();(data/'evidence').write_text('keep')
  release.install(second,sha2,destination);self.assertEqual(release.state(destination)['previous'],sha1)
  release.rollback(destination);self.assertEqual(release.state(destination)['current'],sha1);self.assertEqual((data/'evidence').read_text(),'keep')
  (destination/'versions'/sha2/'preflight').write_text('tampered')
  with self.assertRaises(ValueError):release.rollback(destination)
 def test_unrelated_destination_and_failed_update_are_preserved(self):
  first,sha=self.archive();destination=self.root/'unrelated';destination.mkdir();(destination/'keep').write_text('keep')
  with self.assertRaises(ValueError):release.install(first,sha,destination)
  self.assertEqual((destination/'keep').read_text(),'keep')
  destination=self.root/'installation';release.install(first,sha,destination)
  with self.assertRaises(ValueError):release.install(first,'0'*64,destination)
  self.assertEqual(release.state(destination)['current'],sha)
 def test_standalone_installer_preserves_config_and_data(self):
  import subprocess, sys
  script=self.root/'install.py';script.write_text(release.installer_source())
  prefix=self.root/'application';config=self.root/'settings/preflight.toml';data=self.root/'data';data.mkdir();(data/'keep').write_text('evidence')
  first,sha1=self.archive();second,sha2=self.archive(b'second')
  def run(*args):
   result=subprocess.run([sys.executable,str(script),*map(str,args)],capture_output=True,text=True)
   self.assertEqual(result.returncode,0,result.stderr);return json.loads(result.stdout)
  run('install',first,'--sha256',sha1,'--prefix',prefix,'--config',config,'--data',data)
  original=config.read_bytes()
  credential=config.with_name(config.stem+'.admin-token');secret=credential.read_text().strip()
  self.assertGreaterEqual(len(secret),32);self.assertNotIn(secret,original.decode())
  if sys.platform!='win32':self.assertEqual(credential.stat().st_mode & 0o777,0o600)
  run('install',second,'--sha256',sha2,'--prefix',prefix,'--config',config)
  self.assertEqual(config.read_bytes(),original)
  self.assertEqual(run('rollback','--prefix',prefix)['current'],sha1)
  run('uninstall','--prefix',prefix)
  self.assertFalse(prefix.exists());self.assertEqual(config.read_bytes(),original);self.assertEqual((data/'keep').read_text(),'evidence');self.assertEqual(credential.read_text().strip(),secret)
 def test_installer_rejects_configuration_hidden_inside_binary_prefix(self):
  import subprocess
  script=self.root/'install.py';script.write_text(release.installer_source())
  package,sha=self.archive();prefix=self.root/'application';alias=self.root/'outside';alias.mkdir()
  config=alias/'..'/'application'/'preflight.toml'
  result=subprocess.run([sys.executable,str(script),'install',str(package),'--sha256',sha,'--prefix',str(prefix),'--config',str(config)],capture_output=True,text=True)
  self.assertNotEqual(result.returncode,0);self.assertFalse(prefix.exists())
 def test_release_collection_rejects_bad_checksum_and_tag(self):
  import subprocess
  package,sha=self.archive(platform_info=('macos','aarch64'));package=package.rename(self.root/'preflight-0.1.0-macos-aarch64.tar.gz');package.with_name(package.name+'.sha256').write_text(sha+'  '+package.name+'\n')
  (self.root/'install-preflight.py').write_text(release.installer_source())
  output=self.root/'release'
  script=pathlib.Path(release.__file__).with_name('release-files.py')
  result=subprocess.run([sys.executable,str(script),str(self.root),str(output),'v0.1.0'],capture_output=True,text=True)
  self.assertEqual(result.returncode,0,result.stderr);self.assertTrue((output/'SHA256SUMS').is_file())
  formula=(output/'preflight.rb').read_text();self.assertIn(sha,formula);self.assertIn('https://github.com/MaxLaneDev/preflight/releases/download/v0.1.0/'+package.name,formula)
  self.assertIn(hashlib.sha256((output/'preflight.rb').read_bytes()).hexdigest()+'  preflight.rb',(output/'SHA256SUMS').read_text())
  result=subprocess.run([sys.executable,str(script),str(self.root),str(self.root/'bad-tag'),'v9.0.0'],capture_output=True,text=True)
  self.assertNotEqual(result.returncode,0);self.assertFalse((self.root/'bad-tag').exists())
  package.with_name(package.name+'.sha256').write_text('0'*64)
  result=subprocess.run([sys.executable,str(script),str(self.root),str(self.root/'bad-hash'),'v0.1.0'],capture_output=True,text=True)
  self.assertNotEqual(result.returncode,0);self.assertFalse((self.root/'bad-hash').exists())
 def test_homebrew_formula_requires_matching_release_and_checksum(self):
  import subprocess
  package,sha=self.archive(platform_info=('macos','aarch64'));name='preflight-0.1.0-macos-aarch64.tar.gz';package=package.rename(self.root/name)
  sums=self.root/'SHA256SUMS';sums.write_text(sha+'  '+name+'\n')
  script=pathlib.Path(release.__file__).with_name('homebrew-release.py')
  def run(tag,output):return subprocess.run([sys.executable,str(script),str(self.root),tag,str(self.root/output)],capture_output=True,text=True)
  self.assertEqual(run('v0.1.0','formula.rb').returncode,0)
  self.assertIn(sha,(self.root/'formula.rb').read_text())
  self.assertNotEqual(run('v0.1.0','formula.rb').returncode,0)
  for value in ['v9.0.0','v0.1.0;bad']:
   self.assertNotEqual(run(value,'bad.rb').returncode,0);self.assertFalse((self.root/'bad.rb').exists())
  for content in ['0'*64+'  '+name+'\n',sha+'  '+name+'\n'+sha+'  '+name+'\n']:
   sums.write_text(content);self.assertNotEqual(run('v0.1.0','bad.rb').returncode,0);self.assertFalse((self.root/'bad.rb').exists())
if __name__=='__main__':unittest.main()
