import {modalClose,closeDialog} from '/interactions.mjs';
const terminal=new Set(['complete','partial','failed','cancelled']);
export function installExternalCI({api,el,button,query,copy,notice,target,openRun,openSettings,canUse}){
 let generation=0,timer=null;
 const current=g=>g===generation&&target.isConnected&&!target.hidden;
 const link=(label,url)=>{const a=el('a',label);a.href=url;a.target='_blank';a.rel='noopener noreferrer';return a;};
 const start=(source,provider_run_id,cursor)=>api('ci/sync',{source,request_id:crypto.randomUUID(),provider_run_id:provider_run_id||undefined,cursor:cursor||undefined,max_runs:5,max_pages:2});
 async function wait(project,id,status,alive){
  for(;;){if(!alive())return null;const data=await api('ci/sync-status?'+query({project_id:project,sync_id:id}));if(!alive())return null;
   status.textContent=data.cancel_requested?'Stopping import…':({queued:'Waiting to import…',running:'Importing completed provider results…',complete:'Import complete',partial:'Partial import',failed:'Import failed',cancelled:'Import stopped'})[data.state]||data.state;
   if(data.error)status.textContent+=' · '+data.error;
   if(data.retry_at>Date.now())status.textContent+=' · Retry after '+new Date(data.retry_at).toLocaleTimeString('en-GB',{hour12:false});
   if(terminal.has(data.state))return data;
   await new Promise(resolve=>setTimeout(resolve,1000));
  }
 }
 async function show(project){hide();target.hidden=false;const g=generation;
  if(!canUse()){target.replaceChildren(el('p','External CI import is unavailable on this server.','empty-state'));return;}
  const controls=el('div',undefined,'ci-controls'),source=el('select'),status=el('p','','muted'),credential=el('p','','muted'),imports=el('div',undefined,'ci-imports'),rows=el('div',undefined,'ci-runs');
  let remoteCursor=null,before=null,loading=false,signature=null;
  const sync=button('Import recent runs',async()=>{sync.disabled=true;try{await watch((await start(source.value)).sync_id);}catch(error){if(current(g))notice(error);}finally{sync.disabled=!source.value||configuredSources.find(s=>s.name===source.value)?.credential_status==='missing';}});
  const moreRemote=button('More provider runs',async()=>{moreRemote.disabled=true;try{await watch((await start(source.value,null,remoteCursor)).sync_id);}finally{moreRemote.disabled=false;}});
  const more=button('Load more runs',()=>load(false));
  source.setAttribute('aria-label','CI source');source.append(new Option('Choose a CI source…',''));sync.disabled=true;moreRemote.hidden=true;more.hidden=true;
  let configuredSources=[];source.onchange=()=>{const selected=configuredSources.find(s=>s.name===source.value);sync.disabled=!source.value||selected?.credential_status==='missing';remoteCursor=null;moreRemote.hidden=true;credential.textContent=selected?.credential_status==='missing'?'The configured token environment variable is missing or empty on the server. Configure it before importing.':selected?.auth==='configured'?'Server credential is available. Import recent runs to check provider access.':selected?'Anonymous access: public repositories only.':'';};
  controls.append(source,sync,button('+ Add CI source',()=>openSettings({projectId:project,add:true})),button('Configure sources',()=>openSettings({projectId:project})));target.replaceChildren(controls,credential,status,imports,rows,moreRemote,more);
  const sources=await api('ci/sources?'+query({project_id:project,limit:32}));if(!current(g))return;
  for(const value of sources.sources)source.append(new Option(value.name+' · '+value.provider+' · '+value.repository,value.name));
  configuredSources=sources.sources;if(sources.sources.length){source.value=sources.sources[0].name;source.onchange();}else status.textContent='No CI sources configured for this workspace. Choose Add CI source to connect GitHub Actions or GitLab CI. Provider credentials stay in server environment variables.';
  async function watch(id){
   const stop=button('Stop import',()=>api('ci/cancel',{project_id:project,sync_id:id,actor:'user',reason:'Stopped local import from Web UI'}));controls.append(stop);
   try{const data=await wait(project,id,status,()=>current(g));if(!data)return;remoteCursor=data.result?.next_cursor||null;moreRemote.hidden=!remoteCursor;await load(true);await loadImports();}finally{stop.remove();}
  }
  async function load(reset,silent=false){
   if(loading||!current(g)||(silent&&rows.querySelectorAll('.ci-run-row').length>20))return;loading=true;more.dataset.pageError='';more.disabled=true;if(reset&&!silent){before=null;rows.replaceChildren(el('p','Loading imported runs…','loading-indicator'));}
   try{const data=await api('ci/runs?'+query({project_id:project,before:reset?null:before,limit:20}));if(!current(g))return;if(reset){const nextSignature=JSON.stringify(data);if(silent&&signature===nextSignature)return;signature=nextSignature;rows.replaceChildren();}
    for(const item of data.runs){const row=el('article',undefined,'ci-run-row'),detail=el('div');
     detail.append(el('strong',item.provider+' · '+item.provider_run_id),el('span',item.status+' · '+item.import_state,'muted'),el('code',item.git_sha||'Commit SHA unavailable'));
     row.append(button(item.name,()=>openRun(item)),detail,button('Inspect import',()=>inspect(project,item.run_id)));
     if(item.resumable&&sources.sources.some(s=>s.name===item.source))row.append(button('Continue import',async()=>watch((await start(item.source,item.provider_run_id)).sync_id)));
     rows.append(row);
    }
    before=data.next_cursor;more.hidden=!before;if(!rows.childElementCount)rows.append(el('p','No imported CI runs yet. Import completed provider results above.','empty-state'));
   }catch(error){if(current(g)){more.dataset.pageError=error.message;rows.replaceChildren(el('p',error.message,'error'),button('Retry',()=>load(true)));}}finally{loading=false;more.disabled=false;}
  }
  async function loadImports(){
   const data=await api('ci/imports?'+query({project_id:project,limit:100}));if(!current(g))return;imports.replaceChildren();
   for(const item of data.imports.filter(i=>['queued','running'].includes(i.state))){const row=el('div',undefined,'actions');row.append(el('span',item.source+' · '+(item.cancel_requested?'stopping':item.state)),button('Stop import',async()=>{await api('ci/cancel',{project_id:project,sync_id:item.sync_id,actor:'user',reason:'Stopped from import queue'});await loadImports();}));imports.append(row);}
  }
  await load(true);await loadImports();
  async function refresh(){if(!current(g))return;try{await loadImports();await load(true,true);}catch(error){if(current(g))status.textContent=error.message;}if(current(g))timer=setTimeout(refresh,2000);}
  timer=setTimeout(refresh,2000);
 }
 function hide(){generation++;clearTimeout(timer);timer=null;target.hidden=true;}
 async function inspect(project,run){
  const dialog=el('dialog',undefined,'ci-dialog');dialog.setAttribute('aria-label','External CI import');const header=el('div',undefined,'heading'),body=el('div');
  header.append(el('h2','External CI import'),modalClose(button('Close',()=>closeDialog(dialog))));dialog.append(header,body);dialog.addEventListener('close',()=>dialog.remove());document.body.append(dialog);dialog.showModal();body.append(el('p','Loading import…','loading-indicator'));
  try{const data=await api('ci/run?'+query({project_id:project,run_id:run,limit:20,max_bytes:32768}));if(!dialog.open)return;
   body.replaceChildren(el('p',data.provider+' · '+data.provider_run_id+' · '+data.import_state));if(data.web_url)body.append(link('Open provider run',data.web_url));
   const coverage=el('dl',undefined,'ci-coverage');for(const [key,value]of Object.entries(data.coverage||{})){if(['issues','timestamp_origin','dependencies','test_issue'].includes(key))continue;coverage.append(el('dt',key.replaceAll('_',' ')),el('dd',String(value)));}
   body.append(coverage);for(const issue of data.coverage.issues||[])body.append(el('p',issue.component+': '+issue.reason,'muted'));if(data.coverage.test_issue)body.append(el('p',data.coverage.test_issue,'muted'));
   body.append(button('Copy import metadata',()=>copy(data)));await renderArtifacts(body,project,run,()=>dialog.open);
  }catch(error){if(dialog.open)body.replaceChildren(el('p',error.message,'error'));}
 }
 async function renderArtifacts(parent,project,run,alive){
  const box=el('section',undefined,'ci-artifacts');box.append(el('h3','Provider artifacts'));parent.append(box);let offset=0;
  const load=async()=>{const data=await api('ci/run?'+query({project_id:project,run_id:run,offset,limit:20,max_bytes:32768}));if(!alive()||!box.isConnected)return;
   for(const a of data.artifacts){const row=el('article',undefined,'ci-artifact');row.append(el('strong',a.name||a.artifact_id),el('span',a.state+(a.provider?.expired?' · provider expired':''),'muted'));
    if(a.state==='downloaded'){const download=link('Download ZIP','/api/v1/ci/artifact-file?'+query({project_id:project,run_id:run,artifact_id:a.artifact_id}));download.removeAttribute('target');download.download='';row.append(download,el('code',a.sha256));}
    else if(!['provider_expired','artifact_expired'].includes(a.state)){const fetch=button('Fetch artifact',async()=>{fetch.disabled=true;const status=el('span','Downloading…','muted');row.append(status);
     try{const queued=await api('ci/artifact-fetch',{project_id:project,run_id:run,artifact_id:a.artifact_id,request_id:crypto.randomUUID()});const result=await wait(project,queued.sync_id,status,()=>alive()&&row.isConnected);if(result?.state==='complete'){box.replaceChildren(el('h3','Provider artifacts'));offset=0;await load();}}
     catch(error){status.textContent=error.message;}finally{fetch.disabled=false;}
    });row.append(fetch);}
    box.append(row);
   }
   if(!data.artifacts.length&&offset===0)box.append(el('p','No published artifact metadata.','muted'));
   if(data.next_offset!=null){const more=button('More artifacts',async()=>{offset=data.next_offset;await load();more.remove();});box.append(more);}
  };await load();
 }
 return {show,hide,inspect,renderArtifacts};
}
