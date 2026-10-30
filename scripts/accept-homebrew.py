"""Real Homebrew install/upgrade/service test using an owned local tap and local archive."""
import json,os,pathlib,shutil,socket,subprocess,sys,time,urllib.request,urllib.error
ROOT=pathlib.Path(__file__).resolve().parents[1];os.umask(0o077)
out=ROOT/'.validation'/('homebrew-'+time.strftime('%Y%m%d-%H%M%S'));out.mkdir()
brew=pathlib.Path(shutil.which('brew') or '/opt/homebrew/bin/brew');env={**os.environ,'HOMEBREW_NO_AUTO_UPDATE':'1','HOMEBREW_NO_ANALYTICS':'1','HOMEBREW_NO_INSTALL_CLEANUP':'1','HOMEBREW_NO_ENV_HINTS':'1','HOMEBREW_NO_AUTOREMOVE':'1','XDG_CONFIG_HOME':str(out/'xdg')}
package=json.loads(pathlib.Path(sys.argv[1]).read_text())[0];archive=pathlib.Path(package['archive']).resolve();tap='codex-preflight/acceptance';formula=tap+'/preflight';source=out/'tap';source.mkdir();(source/'Formula').mkdir()
prefix=pathlib.Path(subprocess.check_output([str(brew),'--prefix'],env=env,text=True).strip());settings=prefix/'etc/preflight';log=prefix/'var/log/preflight.log';tapdir=prefix/'Library/Taps/codex-preflight/homebrew-acceptance'
assert not settings.exists() and not log.exists() and not tapdir.exists(),'owned paths must be absent before this test'
assert subprocess.run([str(brew),'list','--formula','preflight'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode!=0,'preflight already installed'
inventory=subprocess.check_output([str(brew),'list','--versions','--formula'],env=env,text=True)
(out/'inventory-before.txt').write_text(inventory)
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base='http://127.0.0.1:'+str(port);opener=urllib.request.build_opener(urllib.request.ProxyHandler({}));tapped=False;installed=False;service=False

def run(*args):
 result=subprocess.run([str(brew),*args],env=env,cwd=ROOT,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,timeout=180)
 with (out/'brew.log').open('a') as output:output.write('$ brew '+' '.join(args)+'\n'+result.stdout+'\n')
 assert result.returncode==0,'brew failed; see '+str(out/'brew.log');return result.stdout

def call(path,body=None):
 req=urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
 with opener.open(req,timeout=5) as response:return json.load(response)
def ready():
 for _ in range(200):
  try:call('/health');return
  except OSError:time.sleep(.1)
 raise RuntimeError('Homebrew service did not become ready')
def stopped():
 for _ in range(100):
  try:
   with socket.create_connection(('127.0.0.1',port),timeout=.1):pass
  except OSError:return
  time.sleep(.1)
 raise RuntimeError('owned Homebrew service still listening')
try:
 subprocess.run([sys.executable,str(ROOT/'scripts/release.py'),'formula',str(archive),'--sha256',package['sha256'],'--url','https://example.invalid/preflight.tar.gz','--homepage','https://example.invalid/preflight','--destination',str(source/'Formula/preflight.rb')],check=True,stdout=subprocess.DEVNULL,env=env)
 text=(source/'Formula/preflight.rb').read_text().replace('https://example.invalid/preflight.tar.gz',archive.as_uri());(source/'Formula/preflight.rb').write_text(text)
 for command in [['git','init',str(source)],['git','-C',str(source),'add','.'],['git','-C',str(source),'-c','user.name=Preflight acceptance','-c','user.email=acceptance@example.invalid','commit','-m','Test local formula']]:subprocess.run(command,check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 run('tap',tap,str(source));tapped=True;run('trust','--formula',formula);run('install','--formula','--build-from-source',formula);installed=True;run('test',formula)
 config=settings/'preflight.toml';credential=settings/'admin-token';key=credential.read_text().strip();assert len(key)>=32 and credential.stat().st_mode & 0o777==0o600
 original=config.read_text();assert 'admin_token_file' in original and key not in original
 data=out/'data';config.write_text(original.replace('127.0.0.1:4170','127.0.0.1:'+str(port)).replace(str(prefix/'var/preflight'),str(data)));saved=config.read_bytes();secret=credential.read_bytes()
 run('services','start',formula);service=True;ready();assert call('/api/v1/server-settings',{'admin_token':key})['admin_token_file_supported'];caps=call('/api/v1/capabilities');assert caps['features']['bundled_web_ui']
 startup_checked=False
 if caps['features'].get('macos_login_startup'):
  settings_data=call('/api/v1/server-settings',{'admin_token':key});startup=settings_data['autostart'];assert startup['available'] and startup['manager']=='homebrew' and startup['enabled']
  target='gui/'+str(os.getuid())+'/'+startup['label'];pid_before=subprocess.check_output(['/bin/launchctl','print',target],text=True).split('pid = ')[1].splitlines()[0]
  changed=call('/api/v1/server-autostart',{'admin_token':key,'expected_revision':settings_data['revision'],'enabled':False});assert not changed['autostart']['enabled'];assert call('/health')
  changed=call('/api/v1/server-autostart',{'admin_token':key,'expected_revision':changed['revision'],'enabled':True});assert changed['autostart']['enabled'] and changed['autostart']['manager']=='homebrew'
  assert subprocess.check_output(['/bin/launchctl','print',target],text=True).split('pid = ')[1].splitlines()[0]==pid_before
  startup_checked=True
 # Upgrade an actual installed formula revision, then explicitly restart only the owned idle service.
 installed_formula=tapdir/'Formula/preflight.rb';installed_formula.write_text(installed_formula.read_text().replace('class Preflight < Formula','class Preflight < Formula\n  revision 1'))
 run('upgrade',formula);assert config.read_bytes()==saved and credential.read_bytes()==secret
 run('services','restart',formula);ready();assert call('/api/v1/server-settings',{'admin_token':key})['admin_token_file_supported']
 run('services','stop',formula);service=False;stopped();run('uninstall','--force',formula);installed=False
 assert config.read_bytes()==saved and credential.read_bytes()==secret and (data/'preflight.sqlite3').is_file()
 run('untap',tap);tapped=False
 after=subprocess.check_output([str(brew),'list','--versions','--formula'],env=env,text=True);(out/'inventory-after.txt').write_text(after);assert sorted(inventory.splitlines())==sorted(after.splitlines()),'other installed formula versions changed'
 receipt={'actual_homebrew_install':True,'web_startup_policy_reuses_homebrew_agent':startup_checked,'formula_test':True,'formula_revision_upgrade':True,'service_start_restart_stop':True,'file_credential_without_shell_environment':True,'config_and_credential_preserved':True,'uninstall_preserves_data':True,'local_file_download':True,'public_https_download':'not tested; no public release exists','production_mutations':False}
 (out/'acceptance.json').write_text(json.dumps(receipt,indent=2)+'\n');print(json.dumps({'out':str(out),**receipt}))
finally:
 if service:run('services','stop',formula);stopped()
 if installed:run('uninstall','--force',formula)
 if tapped:run('untap',tap)
 if settings.exists():
  assert set(p.name for p in settings.iterdir())<={'preflight.toml','admin-token'},'unrelated files found; preserving settings'
  shutil.rmtree(settings)
 if log.exists():log.unlink()
