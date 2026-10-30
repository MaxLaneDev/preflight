"""Cold-start the packaged server on an owned ephemeral port; no host installation."""
import sys,pathlib,tempfile,json,socket,subprocess,time,urllib.request
import release
archives=list(pathlib.Path(sys.argv[1]).glob('preflight-[0-9]*.tar.gz'));assert len(archives)==1
archive=archives[0];sha=archive.with_name(archive.name+'.sha256').read_text().split()[0]
with tempfile.TemporaryDirectory(prefix='preflight-smoke-') as temporary:
 root=pathlib.Path(temporary);binary_root=root/'binary';manifest=release.verify(archive,sha)
 with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
 config=root/'preflight.toml';installer=archive.parent/'install-preflight.py';subprocess.run([sys.executable,str(installer),'install',str(archive),'--sha256',sha,'--prefix',str(binary_root),'--config',str(config),'--data',str(root/'data')],check=True,stdout=subprocess.DEVNULL);config.write_text(config.read_text().replace('127.0.0.1:4170','127.0.0.1:'+str(port)))
 with (root/'server.log').open('wb') as log:p=subprocess.Popen([str(binary_root/'current'/manifest['binary']),'--config',str(config),'serve'],stdout=log,stderr=log)
 try:
  base='http://127.0.0.1:'+str(port);http=urllib.request.build_opener(urllib.request.ProxyHandler({}))
  for _ in range(200):
   assert p.poll() is None,'packaged server exited: '+(root/'server.log').read_text()
   try:http.open(base+'/health',timeout=1).close();break
   except OSError:time.sleep(.05)
  else:raise RuntimeError('packaged server did not become healthy')
  for path in ['/','/app.js','/app.css','/projects/example/pipelines','/executions']:
   with http.open(base+path,timeout=10) as response:assert response.status==200
  with http.open(base+'/api/v1/capabilities',timeout=10) as response:assert json.load(response)['features']['bundled_web_ui']
  import urllib.error
  try:
   http.open(urllib.request.Request(base+'/api/v1/projects',data=json.dumps({'name':'denied','path':str(root)}).encode(),headers={'Content-Type':'application/json'}),timeout=10)
   raise AssertionError('empty allowlist allowed project registration')
  except urllib.error.HTTPError as error:assert error.code==400
 finally:p.terminate();p.wait(timeout=20)
print('Packaged UI/API cold start passed')
