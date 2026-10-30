import pathlib,tempfile,subprocess,json,socket,os,time,urllib.request,plistlib,datetime,sys
root=pathlib.Path(__file__).resolve().parents[1]
binary=pathlib.Path(sys.argv[1]).resolve() if len(sys.argv)>1 else root/'target/release/preflight'
expected=json.loads(subprocess.check_output([str(binary),'--version'],text=True))['schema']
out=root/'.validation'/('service-'+datetime.datetime.now().strftime('%Y%m%d-%H%M%S'));out.mkdir()
with tempfile.TemporaryDirectory(prefix='preflight service & ') as tmp:
 p=pathlib.Path(tmp); sock=socket.socket();sock.bind(('127.0.0.1',0));port=sock.getsockname()[1];sock.close()
 config=p/'config.toml';config.write_text(f"version=1\nbind='127.0.0.1:{port}'\ndata_dir='data'\nallowed_roots=['.']\n")
 result=json.loads(subprocess.check_output([str(binary),'--config',str(config),'service','export','launchd',str(p/'units')],text=True))
 plist=pathlib.Path(result['files'][0]);parsed=plistlib.loads(plist.read_bytes());assert parsed['ProgramArguments'][2]==str(config.resolve())
 for name in result['files']:
  if name.endswith('.plist'):subprocess.run(['plutil','-lint',name],check=True,stdout=subprocess.PIPE)
 domain=f'gui/{os.getuid()}';label=parsed['Label'];loaded=False
 try:
  command=subprocess.run(['launchctl','bootstrap',domain,str(plist)],capture_output=True,text=True)
  (out/'bootstrap.txt').write_text(command.stdout+command.stderr);assert command.returncode==0,command.stderr
  loaded=True
  for _ in range(100):
   try:
    with urllib.request.urlopen(f'http://127.0.0.1:{port}/api/v1/capabilities',timeout=.5) as r:cap=json.load(r)
    break
   except OSError:time.sleep(.1)
  else:raise RuntimeError('launchd server not ready')
  assert cap['schema_version']==expected
  status=subprocess.check_output(['launchctl','print',f'{domain}/{label}'],text=True)
  (out/'status.txt').write_text(status)
  (out/'result.json').write_text(json.dumps({'adapter':'launchd','ready':True,'schema':expected,'path_with_spaces_and_ampersand':True}))
 finally:
  if loaded:
   subprocess.run(['launchctl','bootout',f'{domain}/{label}'],check=True)
   for _ in range(100):
    try:
     with socket.create_connection(('127.0.0.1',port),timeout=.1):pass
    except OSError:break
    time.sleep(.1)
   else:raise RuntimeError('test service still listening')
print(out)
