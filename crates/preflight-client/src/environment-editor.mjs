export function preparedImageChoices(stages,stageId){
 return stages.filter(stage=>stage.id!==stageId).flatMap(stage=>Object.entries(stage.outputs||{}).filter(([,output])=>output.type==='file').map(([name])=>({value:'${artifact.'+stage.id+'.'+name+'}',label:stage.id+' / '+name,producer:stage.id})));
}
export function choosePreparedImage(stage,value){
 if(!value){delete stage.container.image_from;stage.container.image='';return;}
 const match=/^\$\{artifact\.([A-Za-z0-9_-]+)\.([A-Za-z0-9_-]+)\}$/.exec(value);if(!match)throw new Error('Choose a file artifact.');
 delete stage.container.image;stage.container.image_from=value;stage.needs=[...new Set([...(stage.needs||[]),match[1]])];
}
export function renderEnvironment({target,stage,stages,field,el,button,update,rebuild,api,projectId,arch}){
 const setup=el('details',undefined,'stage-environment');setup.open=Boolean(stage.setup?.length);setup.append(el('summary','Environment setup'));
 setup.append(el('p','Commands run in order before the main command, in the same environment. The stage timeout includes setup. Use program “.” with ./script.sh to retain exported variables.','muted'));
 for(const [index,command]of (stage.setup||[]).entries()){
  const row=el('div',undefined,'stage-fields');field(row,'Program',command.program,value=>command.program=value);
  if((command.args||[]).some(value=>value.includes('\n')))row.append(el('p','Edit arguments containing line breaks in Code.','muted'));
  else field(row,'Arguments · one per line',(command.args||[]).join('\n'),value=>command.args=value===''?[]:value.split('\n'),true);
  row.append(button('Remove setup command',()=>{stage.setup.splice(index,1);if(!stage.setup.length)delete stage.setup;update();rebuild();}));setup.append(row);
 }
 setup.append(button('+ Setup command',()=>{stage.setup??=[];stage.setup.push({program:'sh',args:[]});update();rebuild();}));target.append(setup);
 const environment=el('details',undefined,'stage-environment');environment.open=Boolean(stage.container||stage.worker);environment.append(el('summary','Execution environment'));
 const label=el('label','Run on'),host=el('select');host.setAttribute('aria-label','Execution environment');
 host.append(new Option('Local machine','native'),new Option('Local Docker container','docker'));
 if(stage.worker)host.append(new Option('Selected worker · '+(stage.container?'Docker · ':'')+stage.worker.os+' / '+stage.worker.arch,'worker'));
 host.value=stage.worker?'worker':stage.container?'docker':'native';label.append(host);environment.append(label);
 const workers=new Map();
 if(projectId){let cursor=null,loading=false;
 const load=async()=>{if(loading)return;loading=true;host.dataset.loading='true';
  try{const data=await api('workers?project_id='+encodeURIComponent(projectId)+(cursor?'&after='+encodeURIComponent(cursor):''));if(!host.isConnected)return;
   for(const worker of data.workers||[]){workers.set(worker.worker_id,worker);if(![...host.options].some(option=>option.value===worker.worker_id))host.append(new Option(worker.name+' · '+worker.os+' / '+worker.arch+(worker.execution_ready?'':' · offline'),worker.worker_id));if(worker.container_jobs){const key='docker-worker:'+worker.worker_id;workers.set(key,worker);if(![...host.options].some(option=>option.value===key))host.append(new Option(worker.name+' · Docker · '+worker.os+' / '+worker.arch,key));}if(stage.worker?.worker_id===worker.worker_id){const value=(stage.container?'docker-worker:':'')+worker.worker_id;if([...host.options].some(option=>option.value===value))host.value=value;}}
   cursor=data.next_cursor;host.dataset.more=String(Boolean(cursor));delete host.dataset.error;
  }catch(error){host.dataset.error=error.message;}finally{loading=false;host.dataset.loading='false';}
 };host.addEventListener('choices-more',event=>event.detail?.waitUntil?.(load()));load();}

 host.onchange=()=>{
  if(host.value==='docker'){delete stage.worker;stage.executor='docker';stage.container??={image:'',platform:arch==='x86_64'?'linux/amd64':'linux/arm64'};}
  else if(workers.has(host.value)){const worker=workers.get(host.value);if(host.value.startsWith('docker-worker:')){stage.executor='docker';stage.container??={image:'',platform:worker.arch==='x86_64'?'linux/amd64':'linux/arm64'};}else{delete stage.container;stage.executor='native';}stage.worker={worker_id:worker.worker_id,os:worker.os,arch:worker.arch};}
  else if(host.value==='native'){delete stage.container;delete stage.worker;stage.executor='native';}
  update();rebuild();
 };
 if(stage.container){
  const container=stage.container,fields=el('div',undefined,'stage-fields'),sourceLabel=el('label','Image source'),source=el('select');source.setAttribute('aria-label','Container image source');source.append(new Option('Pinned image digest',''));
  for(const choice of preparedImageChoices(stages,stage.id))source.append(new Option(choice.label,choice.value));source.value=container.image_from||'';source.onchange=()=>{choosePreparedImage(stage,source.value);update();rebuild();};sourceLabel.append(source);fields.append(sourceLabel);
  if(!container.image_from)field(fields,'Image digest',container.image||'',value=>container.image=value).placeholder='repository@sha256:…';
  const platformLabel=el('label','Container platform'),platform=el('select');platform.setAttribute('aria-label','Container platform');platform.append(new Option('Linux ARM64','linux/arm64'),new Option('Linux x86_64','linux/amd64'));platform.value=container.platform;platform.onchange=()=>{container.platform=platform.value;update();};platformLabel.append(platform);fields.append(platformLabel);
  field(fields,'User UID or UID:GID',container.user||'',value=>{if(value)container.user=value;else delete container.user;});
  for(const [caption,key,checked]of [['Allow network downloads','network',container.network==='bridge'],['Writable container filesystem','read_only',container.read_only===false]]){const label=el('label',caption),input=el('input');input.type='checkbox';input.checked=checked;input.onchange=()=>{container[key]=key==='network'?(input.checked?'bridge':'none'):!input.checked;update();};label.append(input);fields.append(label);}
  environment.append(fields,el('p','Prepare an image in an upstream stage and select its file artifact. Package installation may require an explicitly configured root user and writable filesystem.','muted'));
 }
 target.append(environment);
}
