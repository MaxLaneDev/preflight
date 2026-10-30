import {installReleaseSettings} from './releases.mjs';
import {installAccessSettings} from './runtime-settings.mjs';
import {installCISettings} from './ci-settings.mjs';
// Server-side filesystem checks and atomic revision checks remain authoritative.
export const settingsGroups = [
 ['Network', [
  {key:'release_repository',label:'Release repository',type:'text',example:'owner/preflight',format:'Public GitHub owner/repository. Empty = no network checks.',help:'Notify about stable product releases and show their changelog. Checked every six hours without a GitHub token. Does not download or install binaries. Changes apply on save.'},
  {key:'admin_token_file',label:'Administrator token file',type:'text',example:'/path/to/preflight/admin-token',format:'Server path to a regular file with owner-only permissions (0600). Empty = environment token only.',help:'Reads the settings credential from a local file. The environment variable PREFLIGHT_ADMIN_TOKEN takes precedence. File contents are never returned. Changing this path requires an existing private file. You may need its token to unlock Settings again.'},
  {key:'bind',label:'API / MCP address',type:'text',required:true,example:'127.0.0.1:4170',format:'Loopback IP:port. IPv6: [::1]:4170. Port: 1–65535.',help:'Address used by clients and MCP. Changing it requires updating client connections.'},
  {key:'worker_bind',label:'Worker gateway address',type:'text',example:'127.0.0.1:4171',format:'Loopback IP:port, different from API / MCP. Empty = disabled.',help:'Separate gateway for remote workers through SSH or a TLS proxy. This does not enroll workers.'}
 ]],
 ['Execution limits', [
  {key:'max_active_runs',label:'Concurrent runs',type:'number',required:true,min:1,max:32,example:'8',format:'Whole number: 1–32 runs. Default: 32.',help:'Server-wide admission limit, including source preparation and runs waiting for command slots. Excess direct submissions return Server busy; planned queues wait to submit.'},
  {key:'max_parallel_commands',label:'Command slots',type:'number',required:true,min:1,max:32,example:'4',format:'Whole number: 1–32 slots. Default: 2.',help:'Shared capacity for stages and source preparation. A stage can reserve multiple slots. Pipeline stage parallelism is configured in each pipeline.'},
  {key:'memory_budget_mib',label:'Scheduling memory budget',type:'number',min:1,example:'8192',format:'Positive whole number in MiB. 1024 MiB = 1 GiB. Empty = disabled.',help:'Admits stages according to declared memory. This is a scheduling budget, not an operating-system RAM limit.'},
  {key:'default_stage_memory_mib',label:'Default stage memory',type:'number',required:true,min:1,example:'256',format:'Positive whole number in MiB. Default: 256. Must fit within the budget.',help:'Reservation for stages without an explicit memory requirement and for source preparation. Used when memory admission is enabled.'}
 ]],
 ['Build disk space', [
  {key:'min_free_disk_bytes',label:'Minimum free disk space',type:'number',min:1,max:9007199254740991,example:'10737418240',format:'Positive whole byte count. 1073741824 bytes = 1 GiB. Empty = disabled.',help:'Rejects new source preparation and stages below this free-space floor. Running commands continue. This is admission, not reserved disk capacity.'},
  {key:'max_storage_mib',label:'Managed storage threshold (MiB)',type:'number',min:1,max:8589934591,example:'102400',format:'Positive whole MiB. 1024 MiB = 1 GiB; 102400 MiB = 100 GiB. Empty = disabled.',help:'Counts file sizes in the server data directory, including workspaces, cache, artifacts, logs, archives and database. At or above this threshold, new preparation and stages are rejected. Running commands continue and may exceed it. This is not a hard filesystem quota. Use cleanup policies to reclaim eligible old data.'},
]],
 ['Storage and project access', [
  {key:'data_dir',label:'Data directory',type:'text',required:true,example:'data',format:'Server path, absolute or relative to the configuration file. No ~ expansion.',help:'Stores the database and execution evidence. A different directory opens a different database after restart; existing history is not migrated.'},
  {key:'allowed_roots',label:'Allowed project directories',type:'lines',required:false,example:'/path/to/projects\nexamples',format:'One server directory per line. Empty = no projects allowed. Every listed directory must exist. Relative paths use the configuration directory.',help:'Projects must be inside these roots. Keep access limited to the source directories you intend to build. Paths are checked on the server.'}
 ]],
 ['Container execution', [
  {key:'docker_command',label:'Docker executable',type:'text',example:'/usr/local/bin/docker',format:'Absolute server executable path. Empty = Docker disabled. Local Unix socket / Windows named pipe only.',help:'Optional pinned-image executor. Configure image, Linux architecture and network in each pipeline stage. Images must already exist locally. Native execution needs no Docker.'}
 ]],
 ['Workers and completion hooks', [
  {key:'ssh_command',label:'SSH executable',type:'text',example:'/usr/bin/ssh',format:'Absolute server file path. Empty = managed SSH connections disabled.',help:'SSH client used to connect to remote computers. Hosts, keys and reconnect policies are configured per connection.'},
  {key:'worker_launcher',label:'Worker launcher executable',type:'text',example:'/path/to/preflight',format:'Absolute server file path. Empty = use the running Preflight executable.',help:'Preflight executable that launches worker installation over SSH. Remote platform compatibility is checked during bootstrap.'},
  {key:'codex_command',label:'Codex executable',type:'text',example:'/opt/homebrew/bin/codex',format:'Absolute server file path. Empty = Codex completion delivery disabled.',help:'Delivers completion events to an explicitly bound Codex chat. Each run still needs its own hook binding.'}
 ]]
];

function address(value){
 const match=/^(127\.\d{1,3}\.\d{1,3}\.\d{1,3}|\[[0-9a-f:]+\]):(\d{1,5})$/i.exec(value);
 if(!match||Number(match[2])<1||Number(match[2])>65535)return null;
 const host=match[1];
 if(host.startsWith('127.'))return host.split('.').every(n=>String(Number(n))===n&&Number(n)<=255)?host+':'+Number(match[2]):null;
 try{return new URL('http://'+value).hostname==='[::1]'?'[::1]:'+Number(match[2]):null;}catch{return null;}
}
export function validateSettings(values){
 const errors={};
 if(values.release_repository&&!/^(?!\.{1,2}\/)[A-Za-z0-9_.-]{1,100}\/(?!\.{1,2}$)[A-Za-z0-9_.-]{1,100}$/.test(values.release_repository))errors.release_repository='Enter a public GitHub repository as owner/repository, without a URL.';
 for(const [,fields]of settingsGroups)for(const field of fields){
  const v=values[field.key],empty=v===undefined||v===null||v===''||(Array.isArray(v)&&!v.length);
  if(empty){if(field.required)errors[field.key]='Enter a value.';continue;}
  if(field.type==='number'&&(!Number.isSafeInteger(Number(v))||Number(v)<field.min||Number(v)>(field.max||Number.MAX_SAFE_INTEGER)))errors[field.key]=field.max?'Enter a whole number from 1 to '+field.max+'.':'Enter a positive whole number within the safe integer range.';
  if(['bind','worker_bind'].includes(field.key)&&!address(v))errors[field.key]='Use a loopback IP and port 1–65535, for example 127.0.0.1:4170 or [::1]:4170.';
  if(['ssh_command','worker_launcher','codex_command','docker_command'].includes(field.key)&&!(/^(?:\/|[A-Za-z]:[\\/]|\\\\[^\\]+\\)/.test(v)))errors[field.key]='Enter an absolute server path, such as /usr/bin/ssh or C:\\Windows\\System32\\OpenSSH\\ssh.exe.';
  if((field.type==='text'||field.type==='lines')&&(Array.isArray(v)?v:[v]).some(p=>/[\x00-\x1f]/.test(p)||p.startsWith('~')||p.includes('://')))errors[field.key]='Use a server path or address, without a URL, control characters or ~ expansion.';
 }
 if(values.worker_bind&&address(values.bind)&&address(values.bind)===address(values.worker_bind))errors.worker_bind='Choose a different address or port from API / MCP.';
 if(values.memory_budget_mib&&Number(values.default_stage_memory_mib)>Number(values.memory_budget_mib))errors.default_stage_memory_mib='Default stage memory must not exceed the scheduling memory budget.';
 return errors;
}
export function validatePolicies(policies){
 const names=new Set();
 return policies.map(p=>{
  const error={};
  if(!/^[A-Za-z0-9_-]{1,64}$/.test(p.name)||names.has(p.name))error.name='Use a unique name: 1–64 ASCII letters, digits, _ or -.';
  names.add(p.name);
  if(!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(p.project_id))error.project_id='Choose a registered project.';
  if(!['stage_workspaces','artifact_expiry','logs','source_snapshots','module_copies','cache_artifacts','run_archives'].includes(p.category))error.category='Choose a supported cleanup category.';
  if(p.category==='run_archives'&&p.pipeline_id)error.category='Archive policies require project scope. Use Archives for manual pipeline cleanup.';
  if(p.quota){
   const {high_bytes:high,low_bytes:low}=p.quota;
   if(!Number.isSafeInteger(high)||high<=0)error.quota_high_bytes='Enter a positive whole byte count, or leave both quota fields empty.';
   if(!Number.isSafeInteger(low)||low<0||!(low<high))error.quota_low_bytes='Enter a whole byte count from 0 up to the high watermark minus 1.';
  }
  if(p.disk_pressure){
   if(p.category==='logs')error.disk_start_bytes='Log expiry reuses SQLite pages; use TTL or logical quota instead of filesystem triggers.';
   const {start_below_bytes:start,stop_at_bytes:stop}=p.disk_pressure;
   if(!Number.isSafeInteger(start)||start<=0)error.disk_start_bytes='Enter a positive whole byte count, or leave both disk fields empty.';
   if(!Number.isSafeInteger(stop)||!(stop>start))error.disk_stop_bytes='Enter a whole byte count greater than the start threshold.';
   if(p.quota){error.disk_start_bytes='Choose a storage size limit or a disk-space trigger for this policy.';error.quota_high_bytes=error.disk_start_bytes;}
  }
  for(const [key,max]of [['ttl_seconds',31536000],['interval_seconds',86400],['batch_limit',100]])if(!Number.isSafeInteger(Number(p[key]))||Number(p[key])<(key==='batch_limit'?1:60)||Number(p[key])>max)error[key]=key==='batch_limit'?'Enter 1–100 items per cleanup batch.':'Enter whole seconds from 60 to '+max+'.';
  return error;
 });
}

export function settingsEqual(left,right){
 const normalized=value=>Array.isArray(value)?value.map(normalized):value&&typeof value==='object'?Object.fromEntries(Object.keys(value).sort().map(key=>[key,normalized(value[key])])):value;
 return JSON.stringify(normalized(left))===JSON.stringify(normalized(right));
}

export function renderSettings({target,data,token,el,button,api,remoteChoices,isCurrent,onSaved,onLock,scope='all',projectId,addCISource=false}){
 target.querySelector('.server-settings-form')?.remove();
 const form=el('form',undefined,'server-settings-form');form.noValidate=true;
 const hot=data.reload?.mode==='hot';
 const values=structuredClone(data.saved),controls=new Map(),policyRows=[];
 const summary=el('div',undefined,'settings-summary');
 summary.append(el('strong',scope==='ci'?'External CI sources':hot?'Live server settings':data.restart_required?'Restart required':'Server configuration'),el('p',hot?(data.reload_required?'The file differs from active settings. Save to apply supported changes.':'Changes apply immediately. Running pipelines keep their admission settings.'):(data.restart_required?'Saved settings differ from the running server. Restart after current work finishes.':'Saved settings match the running server.')),...(scope==='ci'?[]:[el('code',data.path)]));
 form.append(summary);
 if(scope==='all'&&data.release_notifications_supported)installReleaseSettings({form,api,token,el,button,isCurrent});
 let startupToggle=null,startupBusy=false;
 if(scope==='all'&&data.autostart?.supported){
  const startup=el('fieldset',undefined,'settings-autostart');startup.append(el('legend','macOS startup'));
  const label=el('label',undefined,'settings-startup-switch');startupToggle=el('input');startupToggle.type='checkbox';startupToggle.setAttribute('role','switch');startupToggle.setAttribute('aria-label','Start Preflight at login');startupToggle.checked=Boolean(data.autostart.enabled);
  label.append(startupToggle,el('span','Start Preflight at login'));startup.append(label,el('p','Applies at your next macOS login. The current server and running pipelines keep working.','settings-help'));
  const error=el('p',data.autostart.available?'':data.autostart.reason,'settings-field-error');error.setAttribute('role','alert');error.hidden=Boolean(data.autostart.available);startup.append(error);form.append(startup);
  startupToggle.onchange=async()=>{
   if(startupBusy||changed())return;const enabled=startupToggle.checked;startupBusy=true;startupToggle.disabled=true;error.hidden=true;
   try{const result=await api('server-autostart',{admin_token:token.value,expected_revision:data.revision,enabled});if(isCurrent())onSaved(result);}
   catch(problem){if(isCurrent()){startupToggle.checked=Boolean(data.autostart.enabled);error.textContent=problem.message;error.hidden=false;}}
   finally{startupBusy=false;update();}
  };
 }

 function field(parent,definition,value,prefix='setting'){
  const {key,label,type='text',help,format,example,required=false,min,max}=definition;
  const wrapper=el('div',undefined,'settings-field'),id=prefix+'-'+key;
  const caption=el('label',label);caption.htmlFor=id;
  const input=el(type==='lines'?'textarea':type==='select'?'select':'input');input.id=id;input.name=key;
  if(type!=='lines'&&type!=='select')input.type=type;
  if(type==='lines')input.rows=3;
  input.value=Array.isArray(value)?value.join('\n'):value??'';input.required=required;
  input.setAttribute('aria-label',label);input.setAttribute('aria-describedby',id+'-help '+id+'-format '+id+'-error');
  if(example)input.placeholder=example;
  if(type==='number'){input.min=String(min??1);input.max=String(max??Number.MAX_SAFE_INTEGER);input.step='1';input.inputMode='numeric';}
  if(type==='text'){input.spellcheck=false;input.autocomplete='off';}
  const description=el('p',help||'', 'settings-help');description.id=id+'-help';
  const hint=el('p',format||'', 'settings-format');hint.id=id+'-format';
  const error=el('p','', 'settings-field-error');error.id=id+'-error';error.setAttribute('aria-live','polite');error.hidden=true;
  wrapper.append(caption,input,description,hint,error);parent.append(wrapper);
  return {input,error,wrapper,type};
 }
 for(const [title,fields]of settingsGroups){
  if(title==='Container execution'&&!data.runtime_settings)continue;
  const group=el('fieldset');group.append(el('legend',title));
  for(const def of fields){
   if(def.key==='release_repository'&&!data.release_notifications_supported)continue;
   if(def.key==='docker_command'&&!data.runtime_settings)continue;
   if(def.key==='admin_token_file'&&!data.admin_token_file_supported)continue;
   if(def.key==='min_free_disk_bytes'&&!data.disk_admission)continue;
   if(def.key==='max_storage_mib'&&!data.storage_size_admission)continue;
   const control=field(group,def,values[def.key]);controls.set(def.key,control);
   if(hot&&data.reload.immutable_fields.includes(def.key)){
    control.input.readOnly=true;control.wrapper.classList.add('settings-startup-only');
    control.wrapper.querySelector('label').append(el('span','Startup only','settings-field-badge'));
    control.wrapper.querySelector('.settings-help').append(' Fixed while the server is running; hot reload does not replace listeners or move the database.');
   }
  }
  form.append(group);
 }
 const retention=el('fieldset',undefined,'settings-retention');retention.append(el('legend','Automatic storage cleanup'),el('p','Policies expire only their selected payload category. Pins, modules, baseline owners and active operations protect data. Failure logs and result metadata are retained. Log expiry reuses database pages; it does not shrink the SQLite file. Cache eviction invalidates selected entries before removing rebuildable outputs. '+(hot?'Changes apply on save. An in-flight cleanup page may finish.':'Changes take effect after restart.'),'settings-help'));
 const policies=el('div'),empty=el('p','No automatic cleanup policies.','muted');policies.append(empty);
 const add=button('+ Add cleanup policy',()=>addPolicy({name:'',project_id:'',pipeline_id:null,category:'stage_workspaces',ttl_seconds:604800,interval_seconds:3600,batch_limit:25}));
 retention.append(policies,add);form.append(retention);
 let serial=0;
 async function populateProjects(select,selected){
  try{
   const page=await api('catalog?kind=workspaces&limit=25');if(!form.isConnected||!isCurrent())return;
   select.replaceChildren(new Option('Choose project…',''));
   for(const row of page.items)select.append(new Option(row.name,row.id));
   if(selected&&!page.items.some(p=>p.id===selected)){
    const project=await api('projects/'+encodeURIComponent(selected));if(!form.isConnected||!isCurrent())return;
    select.append(new Option(project.name,selected));
   }
   select.value=selected||'';remoteChoices(select,'workspaces',page.next_cursor);select.dataset.loading='false';
  }catch{
   if(!form.isConnected)return;select.dataset.loading='false';select.dataset.error='Unable to load projects';remoteChoices(select,'workspaces');
  }
 }
 function addPolicy(policy){
  const card=el('section',undefined,'settings-policy'),header=el('div',undefined,'settings-policy-heading'),row={policy:structuredClone(policy),fields:new Map()};
  header.append(el('h3','Storage cleanup'),button('Remove policy',()=>{policyRows.splice(policyRows.indexOf(row),1);card.remove();update();}));card.append(header);
  const definitions=[
   {key:'category',label:'Cleanup category',type:'select',required:true,format:'Cleanup permanently removes eligible files. History and SHA remain. Archive cleanup keeps one latest copy for the project and all pins; pin other recovery copies you need.'},
   {key:'name',label:'Policy name',required:true,format:'Unique name: 1–64 ASCII letters, digits, _ or -.',example:'weekly-workspaces'},
   {key:'project_id',label:'Project',type:'select',required:true,format:policy.pipeline_id?'Project and pipeline scope are fixed for this existing policy.':'Choose a registered project. Search to find more projects.',help:policy.pipeline_id?'Registered project governed by this pipeline-scoped policy.':'New policies cover all pipelines in the selected project.'},
   {key:'ttl_seconds',label:'Keep completed data for (seconds)',type:'number',required:true,min:60,max:31536000,format:'60–31536000 seconds. 86400 = 1 day; 604800 = 7 days.'},
   {key:'interval_seconds',label:'Check every (seconds)',type:'number',required:true,min:60,max:86400,format:'60–86400 seconds. 3600 = 1 hour.'},
   {key:'batch_limit',label:'Items per cleanup pass',type:'number',required:true,min:1,max:100,format:'Whole number: 1–100. Small batches limit disk activity.'}
  ];
  for(const def of definitions)row.fields.set(def.key,field(card,def,policy[def.key],'policy-'+(++serial)));
  if(data.storage_policy_quotas){
   const quota=el('details',undefined,'settings-quota'),quotaFields=el('div',undefined,'settings-quota-fields');quota.open=Boolean(policy.quota);quota.append(el('summary','Optional storage size limit'),quotaFields);
   for(const def of [
    {key:'quota_high_bytes',label:'Start cleanup above (bytes)',type:'number',min:1,max:Number.MAX_SAFE_INTEGER,format:'Leave both fields empty for TTL only. 1073741824 = 1 GiB. Counts known files in this policy scope, not free disk space.'},
    {key:'quota_low_bytes',label:'Stop cleanup at (bytes)',type:'number',min:0,max:Number.MAX_SAFE_INTEGER,format:'0 up to the high watermark minus 1. Recent and protected data may prevent reaching this target. TTL still applies.'}
   ])row.fields.set(def.key,field(quotaFields,def,policy.quota?.[def.key==='quota_high_bytes'?'high_bytes':'low_bytes'],'policy-'+(++serial)));
   card.append(quota);
  }

  if(data.storage_policy_disk_pressure){
   const disk=el('details',undefined,'settings-quota'),fields=el('div',undefined,'settings-quota-fields');disk.open=Boolean(policy.disk_pressure);disk.append(el('summary','Optional disk-space trigger'),fields);
   for(const def of [
    {key:'disk_start_bytes',label:'Start when available below (bytes)',type:'number',min:1,max:Number.MAX_SAFE_INTEGER,format:'Space available to the server on the data-directory filesystem. Leave both fields empty to disable this trigger. Choose this or a size limit.'},
    {key:'disk_stop_bytes',label:'Stop when available reaches (bytes)',type:'number',min:2,max:Number.MAX_SAFE_INTEGER,format:'Greater than start, within filesystem capacity. 1073741824 = 1 GiB. TTL and protections apply; other applications can consume disk space.'}
   ])row.fields.set(def.key,field(fields,def,policy.disk_pressure?.[def.key==='disk_start_bytes'?'start_below_bytes':'stop_at_bytes'],'policy-'+(++serial)));
   card.append(disk);
  }

  const category=row.fields.get('category').input;for(const value of data.storage_policy_categories||['stage_workspaces'])category.append(new Option(({artifact_expiry:'Expire artifact files',stage_workspaces:'Disposable stage workspaces',logs:'Expire log payloads',source_snapshots:'Expire source snapshots',module_copies:'Unused module copies',cache_artifacts:'Evict rebuildable cache outputs',run_archives:'Expire archive files (keeps latest and pinned copies)'})[value]||value,value));category.value=policy.category;
  const project=row.fields.get('project_id').input;project.append(new Option('Choose project…',''));if(policy.project_id)project.append(new Option('Loading selected project…',policy.project_id));project.value=policy.project_id||'';project.dataset.loading='true';
  if(policy.pipeline_id){project.disabled=true;card.append(el('p','Existing pipeline scope: '+policy.pipeline_id+'. Project and scope are preserved.','settings-format'));}
  policies.append(card);policyRows.push(row);populateProjects(project,policy.project_id);update();
 }
 const ciSettings=installCISettings({form,values,data,field,el,button,populateProjects,projectId,addNew:addCISource,onChange:()=>{validate();update();}});
 const accessSettings=installAccessSettings({form,values,enabled:data.runtime_settings,field,el,button,onChange:()=>{validate();update();}});
 const scopeHelp=el('details',undefined,'settings-scope');scopeHelp.append(el('summary','Where other settings belong'),el('p','Pipeline editor: stage parallelism, timeouts, test adapters, cache inputs and acceptance criteria. Worker connections: hosts, architecture, SSH keys and reconnect policy. Completion hooks: per-run destination chat. Manual cleanup, archives and Trash: workspace / pipeline / run management. Administrator credentials are configured in the server environment.'));
 form.append(scopeHelp);
 const active=el('details',undefined,'settings-active');active.append(el('summary','Active server values'));
 const activeList=el('dl');for(const [,fields]of settingsGroups)for(const def of fields){if(def.key==='release_repository'&&!data.release_notifications_supported)continue;activeList.append(el('dt',def.label),el('dd',Array.isArray(data.active[def.key])?data.active[def.key].join('\n'):String(data.active[def.key]??'Disabled')));}activeList.append(el('dt','Active cleanup policies'),el('dd',String((data.active.storage_policies||[]).length)));active.append(activeList);form.append(active);
 const footer=el('div',undefined,'settings-actions'),dirty=el('span','No unsaved changes.','settings-draft-status');dirty.setAttribute('role','status');
 const save=el('button',hot?'Save settings':'Save for next restart','primary');save.type='submit';
 const discard=button('Discard changes',()=>renderSettings({target,data,token,el,button,api,remoteChoices,isCurrent,onSaved,onLock,scope,projectId,addCISource:false}));
 footer.append(dirty,save,discard,button('Lock settings',onLock));form.append(footer);target.append(form);
 function read(){
  const result=structuredClone(values);
  for(const [key,{input,type}]of controls){const value=input.value.trim();if(value===''){delete result[key];continue;}result[key]=type==='lines'?value.split('\n').map(s=>s.trim()).filter(Boolean):type==='number'?Number(value):value;}
  result.storage_policies=policyRows.map(({policy,fields})=>{const next={...policy};for(const [key,{input,type}]of fields)next[key]=type==='number'?(input.value===''?null:Number(input.value)):input.value.trim();for(const [key,a,b,x,y]of [['quota','quota_high_bytes','quota_low_bytes','high_bytes','low_bytes'],['disk_pressure','disk_start_bytes','disk_stop_bytes','start_below_bytes','stop_at_bytes']])if(fields.has(a)){const first=next[a],second=next[b];delete next[a];delete next[b];if(first===null&&second===null)delete next[key];else next[key]={[x]:first,[y]:second};}return next;});
  if(!result.storage_policies.length)delete result.storage_policies;
  result.ci_sources=ciSettings.read();if(!result.ci_sources.length)delete result.ci_sources;
  if(data.runtime_settings){result.access_tokens=accessSettings.read();if(!result.access_tokens.length)delete result.access_tokens;}
  return result;
 }
 function changed(){return (hot&&data.reload_required)||!settingsEqual(read(),values);}
 function update(){empty.hidden=policyRows.length>0;add.disabled=policyRows.length>=32;if(!footer.isConnected)return;const pending=changed();if(startupToggle){startupToggle.disabled=startupBusy||pending||!data.autostart.available;startupToggle.title=pending?'Save or discard configuration changes first.':'';}save.disabled=!pending;discard.disabled=!pending;dirty.textContent=pending?(hot?'Unsaved changes · apply live.':'Unsaved changes · apply after restart.'):'No unsaved changes.';}
 function showError(control,message){control.error.textContent=message||'';control.error.hidden=!message;control.input.setCustomValidity(message||'');control.input.setAttribute('aria-invalid',String(Boolean(message)));}
 function validate(){const next=read(),errors=validateSettings(next),policyErrors=validatePolicies(next.storage_policies||[]);if(hot)for(const key of data.reload.immutable_fields)delete errors[key];for(const [key,control]of controls)showError(control,errors[key]);policyRows.forEach((row,i)=>{for(const [key,control]of row.fields)showError(control,policyErrors[i][key]);});return accessSettings.validate()&&ciSettings.validate()&&!Object.keys(errors).length&&policyErrors.every(e=>!Object.keys(e).length);}
 form.addEventListener('input',()=>{validate();update();});form.addEventListener('change',()=>{validate();update();});
 form.onsubmit=async event=>{
  event.preventDefault();if(save.disabled||form.getAttribute('aria-busy')==='true')return;
  if(!validate()){const first=form.querySelector('[aria-invalid="true"]');(document.getElementById(first?.id+'-control')||first)?.focus();return;}
  form.querySelector('.settings-save-error')?.remove();
  const configuration=read(),disabled=[...form.querySelectorAll('input,textarea,select,button')].map(input=>[input,input.disabled]);
  form.setAttribute('aria-busy','true');dirty.textContent='Saving configuration…';for(const [input]of disabled)input.disabled=true;
  try{const result=await api('server-settings',{admin_token:token.value,expected_revision:data.revision,configuration});if(isCurrent())onSaved(result);}
  catch(error){if(isCurrent()){const alert=el('p',error.message,'settings-save-error');alert.setAttribute('role','alert');footer.prepend(alert);}}
  finally{form.removeAttribute('aria-busy');for(const [input,value]of disabled)input.disabled=value;update();}
 };
 for(const policy of values.storage_policies||[])addPolicy(policy);if(scope==='ci'){for(const child of form.children)if(child.tagName==='FIELDSET'&&!child.classList.contains('settings-ci'))child.hidden=true;scopeHelp.hidden=true;active.hidden=true;}update();
}
