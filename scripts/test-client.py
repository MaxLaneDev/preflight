import os,json,pathlib,tempfile,subprocess,socket,time,urllib.request,datetime,secrets,sys
repo=pathlib.Path(__file__).resolve().parents[1];build=repo/'target'/os.environ.get('PREFLIGHT_TEST_PROFILE','release');out=repo/'.validation'/('client-'+datetime.datetime.now().strftime('%Y%m%d-%H%M%S')+('-bundled' if '--bundled' in sys.argv else '-compat'));out.mkdir(parents=True)
def port():
 with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
def call(base,path,body=None):
 req=urllib.request.Request(base+'/api/v1/'+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
 with urllib.request.urlopen(req,timeout=10) as r:return json.load(r)
def wait(url,proc):
 for _ in range(100):
  assert proc.poll() is None,'process exited'
  try:
   with urllib.request.urlopen(url,timeout=1):return
  except OSError:time.sleep(.05)
 raise RuntimeError('not ready')
with tempfile.TemporaryDirectory(prefix='preflight-client-accept-') as raw:
 root=pathlib.Path(raw).resolve();admin=port();ui=port();base=f'http://127.0.0.1:{admin}';client=f'http://127.0.0.1:{ui}'
 config=root/'server.toml';config.write_text(f'version=1\nbind="127.0.0.1:{admin}"\ndata_dir={json.dumps(str(root/"data"))}\nallowed_roots=[{json.dumps(str(root))}]\nmax_parallel_commands=4\n')
 processes=[];admin_token=secrets.token_urlsafe(32)
 try:
  with open(out/'server.log','wb') as log:p=subprocess.Popen([build/'preflight','--config',config,'serve'],cwd=repo,stdout=log,stderr=log,start_new_session=True,env={**os.environ,'PREFLIGHT_ADMIN_TOKEN':admin_token});processes.append(p)
  wait(base+'/health',p)
  for label,fail in [('A',False),('B',True)]:
   project=root/label;project.mkdir();(project/'test.rs').write_text('fn sample() {\n    assert!(true);\n}\n')
   child='<failure message="expected failure"/>' if fail else ''
   (project/'template.xml').write_text(f'<testsuite name="suite"><testcase name="sample" classname="module" file="test.rs" line="1">{child}</testcase></testsuite>')
   registered=call(base,'projects',{'name':'Project '+label,'path':str(project)})
   definition={'version':1,'pipeline':{'stages':[{'id':'prepare','command':{'program':'sh','args':['-c','printf "searchable acceptance marker\\n"; printf output > result.bin'],'timeout_seconds':5},'outputs':{'result':{'path':'result.bin','type':'file'}}},{'id':'tests','kind':'test','needs':['prepare'],'command':{'program':'sh','args':['-c','cp template.xml report.xml'],'timeout_seconds':5},'junit':['report.xml']},{'id':'lint','needs':['prepare'],'command':{'program':'sh','args':['-c','true'],'timeout_seconds':5}}]}}
   call(base,'pipeline-configs',{'project_id':registered['id'],'name':'test-suite','expected_revision':0,'definition':definition})
   run=call(base,'saved-pipeline-runs',{'project_id':registered['id'],'name':'test-suite','revision':1})
   for _ in range(100):
    result=call(base,'runs/'+run['id'])
    if result['finished_at'] is not None:break
    time.sleep(.05)
   assert result['status']==('failed' if fail else 'succeeded'),result
   archived=call(base,'logs-archive',{'project_id':registered['id'],'run_id':run['id']})
   assert archived['complete'] is True,archived
   if not fail:
    time.sleep(.002)
    plan=call(base,'artifact-expiry-preview',{'project_id':registered['id'],'expire_artifacts':True,'finished_before':int(time.time()*1000)})
    expired=call(base,'storage-gc-execute',{'project_id':registered['id'],'plan_id':plan['plan_id'],'request_id':'browser-expiry','actor':'test','reason':'verify visible artifact expiry'})
    assert expired['items'][0]['state']=='deleted',expired

  parallel=root/'parallel';parallel.mkdir()
  registered=call(base,'projects',{'name':'Project Parallel','path':str(parallel)})
  definition={'version':1,'pipeline':{'stages':[{'id':'compiler','command':{'program':'sh','args':['-c','true'],'timeout_seconds':5}}]+[{'id':f'group-{i:02}','needs':['compiler'],'command':{'program':'sh','args':['-c','true'],'timeout_seconds':5}} for i in range(1,11)]}}
  call(base,'pipeline-configs',{'project_id':registered['id'],'name':'parallel-check','expected_revision':0,'definition':definition})
  for _ in range(2):
   run=call(base,'saved-pipeline-runs',{'project_id':registered['id'],'name':'parallel-check','revision':1})
   for attempt in range(100):
    result=call(base,'runs/'+run['id'])
    if result['finished_at'] is not None:break
    time.sleep(.05)
   assert result['status']=='succeeded',result

  patchroot=root/'patch';patchroot.mkdir()
  def git(*args):return subprocess.check_output(['git','-C',str(patchroot),*args],text=True).strip()
  git('init','-q');git('config','user.name','Preflight test');git('config','user.email','test@example.invalid')
  (patchroot/'sample.txt').write_text('before\n');git('add','.');git('commit','-qm','baseline');commit=git('rev-parse','HEAD')
  (patchroot/'sample.txt').write_text('after <script>alert(1)</script>\n');patch=git('diff')+'\n'
  registered=call(base,'projects',{'name':'Project Patch','path':str(patchroot)})
  baseline_run=call(base,'git-pipeline-runs',{'project_id':registered['id'],'definition':{'version':1,'pipeline':{'stages':[{'id':'check','command':{'program':'sh','args':['-c','test -f sample.txt'],'timeout_seconds':5}}]}},'source':{'commit':commit,'patches':[]}})
  for _ in range(100):
   baseline_result=call(base,'runs/'+baseline_run['id'])
   if baseline_result['finished_at'] is not None:break
   time.sleep(.05)
  assert baseline_result['status']=='succeeded',baseline_result
  run=call(base,'git-pipeline-runs',{'project_id':registered['id'],'definition':{'version':1,'pipeline':{'stages':[{'id':'check','command':{'program':'sh','args':['-c','test -f sample.txt'],'timeout_seconds':5}}]}},'source':{'commit':commit,'patches':[{'name':'source-change','diff':patch}]}})
  for _ in range(100):
   result=call(base,'runs/'+run['id'])
   if result['finished_at'] is not None:break
   time.sleep(.05)
  assert result['status']=='succeeded',result

  if '--bundled' in sys.argv:client=base
  else:
   with open(out/'client.log','wb') as log:p=subprocess.Popen([build/'preflight-client','--server',base,'--bind',f'127.0.0.1:{ui}'],cwd=repo,stdout=log,stderr=log,start_new_session=True);processes.append(p)
   wait(client,p)
  env=os.environ.copy();env.update(PF_CLIENT_URL=client,PF_EVIDENCE=str(out),PF_TEST_ADMIN_TOKEN=admin_token,PF_BUNDLED=str('--bundled' in sys.argv).lower())
  browser_process=subprocess.Popen(['node',str(repo/'scripts/test-client.cjs')],env=env,cwd=repo,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True);processes.append(browser_process)
  try:stdout,stderr=browser_process.communicate(timeout=240)
  except subprocess.TimeoutExpired:
   browser_process.terminate();stdout,stderr=browser_process.communicate(timeout=10)
   (out/'browser.log').write_text(stdout+stderr);raise RuntimeError('Browser acceptance timed out; requested browser cleanup')
  (out/'browser.log').write_text(stdout+stderr);print(stdout+stderr);assert browser_process.returncode==0
 finally:
  for p in reversed(processes):
   if p.poll() is None:p.terminate()
   try:p.wait(timeout=5)
   except subprocess.TimeoutExpired:p.kill();p.wait()
  (out/'cleanup.json').write_text(json.dumps({'processes':[{'pid':p.pid,'exit':p.returncode} for p in processes]}))
pathlib.Path('/private/tmp/preflight-client-evidence-path').write_text(str(out));print('Evidence:',out)
