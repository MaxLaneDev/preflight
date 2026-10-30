import {bindPageChoices} from './virtual-list.mjs';
import {modalClose,closeDialog} from '/interactions.mjs';
import {msg as uiMessage,attr as uiAttribute,choice as uiOption,uiNode,uiError,formatted as uiFormatted,statusLabel as uiStatus} from '/localization.mjs';
export async function openArchives({api,el,button,query,notice,project_id,pipeline_key,run_id}){
 const dialog=el('dialog',undefined,'archives-dialog');uiAttribute(dialog,'aria-label',uiMessage("Run archives"));
 const heading=el('div',undefined,'heading');heading.append(el('h2',uiMessage("Archives")),modalClose(button(uiMessage("Close"),()=>closeDialog(dialog))));
 const intro=el('p',uiMessage("Portable snapshots of run data, logs and files. Originals stay unchanged."),'muted'),options=el('section',undefined,'archive-options'),list=el('section',undefined,'archive-list');list.setAttribute('aria-live','polite');
 const content=el('div',undefined,'archive-content');content.append(intro,options,list);dialog.append(heading,content);document.body.append(dialog);dialog.showModal();let timer,closed=false,next=null,generation=0;const pending=new Map();
 let encryptionEnabled=false,identityRefs=[];
 function identityChoice(title,auto=false){const label=el('label',title),select=el('select');select.dataset.autoIdentity=String(auto);label.title='Server environment reference to a private identity file; key material is never sent by the browser.';label.append(select);populateIdentities(select);return {label,select};}
 function populateIdentities(select){const value=select.value;select.replaceChildren(uiOption(identityRefs.length?'Select a server identity…':'No server identities configured',''),...identityRefs.map(name=>uiOption(name.slice('PREFLIGHT_ARCHIVE_IDENTITY_'.length),name)));select.value=identityRefs.includes(value)?value:select.dataset.autoIdentity==='true'&&identityRefs.length===1?identityRefs[0]:'';}
 const recipientLabel=el('label','Encryption recipient (optional)'),recipient=el('input');recipient.placeholder='age1… public recipient';recipient.maxLength=128;recipient.spellcheck=false;recipient.autocomplete='off';recipientLabel.title='Encrypt the saved archive to a public age X25519 recipient. Keep the private identity separately.';recipientLabel.hidden=true;recipientLabel.append(recipient);
 const importIdentity=identityChoice('Decryption identity (encrypted archive)');importIdentity.label.hidden=true;
 const importing=el('details'),importForm=el('div',undefined,'archive-options');importing.hidden=true;let retentionEnabled=false;const supported=api('capabilities').then(cap=>{if(closed)return;retentionEnabled=!!cap.features?.archive_retention;encryptionEnabled=!!cap.features?.archive_encryption;identityRefs=cap.features?.archive_identity_refs||[];recipientLabel.hidden=!encryptionEnabled;importIdentity.label.hidden=!encryptionEnabled;file.accept=encryptionEnabled?'.zst,.age,application/zstd,application/octet-stream':'.zst,application/zstd';populateIdentities(importIdentity.select);importing.hidden=!cap.features?.archive_import;cleanup.hidden=!retentionEnabled||!!run_id;}).catch(()=>{});importing.append(el('summary','Import archive'),importForm);options.before(importing);
 const fileLabel=el('label','Archive file (.tar.zst or .tar.zst.age)'),file=el('input');file.type='file';file.accept='.zst,application/zstd';fileLabel.append(file);
 const hashLabel=el('label','Expected SHA256'),hash=el('input');hash.type='text';hash.placeholder='64 lowercase hexadecimal characters';hash.pattern='[0-9a-f]{64}';hash.autocomplete='off';hash.spellcheck=false;hashLabel.append(hash);
 let importRequest=null;
 const upload=button('Import archive',async()=>{
  const selected=file.files?.[0];if(!selected||!hash.checkValidity()){hash.reportValidity();return;}
  if(selected.size>1024*1024*1024)throw new Error('Archive exceeds 1 GiB.');
  importRequest??={project_id,...(importIdentity.select.value?{identity_file_env:importIdentity.select.value}:{}),sha256:hash.value.trim(),request_id:crypto.randomUUID(),actor:'user',reason:'Import archive from Web UI'};
  const response=await fetch('/api/v1/archive/import?'+query(importRequest),{method:'POST',headers:{'Content-Type':'application/octet-stream'},body:selected});
  const result=await response.json();if(!response.ok)throw new Error(result.error?.message||'Archive import failed.');
  importRequest=null;if(closed)return;list.prepend(archiveRow(result));notice('Archive imported and verified.');
 });hash.required=true;upload.disabled=true;const syncImport=()=>{importRequest=null;upload.disabled=!file.files?.length||!/^[0-9a-f]{64}$/.test(hash.value.trim());};file.onchange=()=>{if(file.files?.[0]?.name.endsWith('.age')&&identityRefs.length===1)importIdentity.select.value=identityRefs[0];else importIdentity.select.value='';importIdentity.select.dispatchEvent(new Event('change',{bubbles:true}));syncImport();};hash.oninput=syncImport;importIdentity.select.onchange=syncImport;
 importForm.append(fileLabel,hashLabel,importIdentity.label,el('p','Use the checksum supplied by the archive owner. Imported files keep their original project and run identities; no run is added to this project.','muted'),upload);
 const cleanup=el('details'),cleanupForm=el('div',undefined,'archive-options'),review=el('section');cleanup.hidden=true;cleanup.append(el('summary','Archive retention'),cleanupForm,review);options.before(cleanup);
 cleanupForm.append(el('p',pipeline_key?'Scope: this pipeline’s archives.':'Scope: this project’s archives.','muted'));
 const fields={};for(const [key,title,value,min,max]of [['days','Delete archives older than (days)','30',0,36500],['keep','Keep latest archives','1',1,100],['gib','Stop at retained archive size (GiB, optional)','',0,8388607]]){const label=el('label',title),input=el('input');input.type='number';input.value=value;input.min=min;input.max=max;input.step=key==='gib'?'any':'1';input.required=key!=='gib';label.append(input);cleanupForm.append(label);fields[key]=input;}
 cleanupForm.append(el('p','Pinned and active archives are protected. Only archive files are deleted; original runs, artifacts and restored files stay unchanged. Size targets are soft: the age and latest-copy protections still apply.','muted'));
 let checkedPlan=null,executeRequest=null,cleanupCursor=null;
 const erase=button('Delete archive files',async()=>{
  if(!checkedPlan)return;executeRequest??={project_id,plan_id:checkedPlan.plan_id,request_id:crypto.randomUUID(),actor:'user',reason:'Manual archive retention from Web UI'};
  const result=await api('storage-gc-execute',executeRequest);if(closed)return;
  if(!result.complete){erase.textContent='Retry blocked cleanup';notice('Some archive files are in use. Retry this cleanup after readers finish.');return;}
  executeRequest=null;checkedPlan=null;erase.disabled=true;previewCleanup.disabled=false;review.replaceChildren(el('p',`Removed ${result.deleted_file_bytes.toLocaleString('en')} archive file bytes.`));if(result.target_met===false)review.append(el('p','Size target not reached: protected or recent archives remain.','muted'));if(result.target_bytes!==null&&result.target_met===null)review.append(el('p','Retained size could not be verified. No quota completion is claimed.','muted'));await refresh();notice('Archive cleanup completed.');
 });erase.className='danger';erase.disabled=true;
 const previewCleanup=button('Preview cleanup',async()=>{
  for(const input of Object.values(fields))if(!input.reportValidity())return;
  const days=fields.days.valueAsNumber,keep=fields.keep.valueAsNumber;if(!Number.isInteger(days)||!Number.isInteger(keep))throw new Error('Days and latest-copy count must be whole numbers.');
  const target=fields.gib.value===''?null:Math.floor(fields.gib.valueAsNumber*1073741824);
  const plan=await api('archive/cleanup/preview',{project_id,pipeline_key,finished_before:Date.now()-days*86400000,target_bytes:target,keep_latest:keep,after:cleanupCursor,limit:20});if(closed)return;
  checkedPlan=plan;executeRequest=null;cleanupCursor=plan.next_cursor;review.replaceChildren();let eligible=0;
  for(const item of plan.entries){if(item.eligible)eligible++;review.append(el('p',`${item.archive_id.slice(-8)} · ${item.eligible?'eligible':item.reason} · ${item.file_bytes.toLocaleString('en')} bytes`,'muted'));}
  review.prepend(el('strong',`${eligible} archive ${eligible===1?'file':'files'} · ${plan.estimated_file_bytes.toLocaleString('en')} bytes`),el('p','Before '+new Date(plan.request.finished_before).toLocaleString('en-GB',{hour12:false}),'muted'));if(plan.next_cursor)review.append(el('p','More candidates remain. Preview the next batch after completing this one.','muted'));
  erase.textContent=`Delete ${eligible} archive ${eligible===1?'file':'files'}`;erase.disabled=!eligible;previewCleanup.disabled=eligible>0;
 });
 for(const input of Object.values(fields))input.oninput=()=>{checkedPlan=null;executeRequest=null;cleanupCursor=null;erase.disabled=true;previewCleanup.disabled=false;review.replaceChildren();};
 cleanupForm.append(previewCleanup,erase);
 dialog.addEventListener('close',()=>{closed=true;clearTimeout(timer);pending.clear();dialog.remove();});
 const label=el('label',uiMessage("Completed run")),select=el('select');uiAttribute(select,'aria-label',uiMessage("Run to archive"));label.append(select);options.append(label);
 let cursor=null;const older=button(uiMessage("Load older runs"),()=>loadRuns(true));options.append(older);bindPageChoices(older,[select]);
 async function loadRuns(append=false){const data=await api('executions?'+query({project_id,view:'recent',kind:'all',after:append?cursor:null,limit:25}));if(closed)return;
  if(!append)select.replaceChildren(uiOption(uiMessage("Choose a completed run…"),''));
  for(const item of data.items||[])if(item.target_kind==='run'&&(!pipeline_key||item.pipeline_key===pipeline_key)&&![...select.options].some(o=>o.value===item.target_id))select.append(uiOption(uiMessage("{0} · {1} · {2} · {3}",{0:item.name,1:item.target_id.slice(-8),2:uiStatus(item.state),3:uiFormatted(new Date(item.at_ms),"toLocaleString")}),item.target_id));
  cursor=data.next_cursor;older.hidden=!cursor;if(run_id&&!append){if(![...select.options].some(o=>o.value===run_id))select.append(uiOption(uiMessage("Run {0}",{"0":run_id.slice(-8)}),run_id));select.value=run_id;}syncCreate();
 }
 const choices=el('div',undefined,'archive-components'),inputs={};
 for(const [key,title,checked]of [['logs',uiMessage("Logs"),true],['artifacts',uiMessage("Artifacts"),true],['source',uiMessage("Source snapshot"),true],['environment',uiMessage("Working environment files"),false]]){const wrap=el('label'),input=el('input');input.type='checkbox';input.checked=checked;inputs[key]=input;wrap.append(input,uiNode(title));choices.append(wrap);}
 options.append(choices,recipientLabel,el('p',uiMessage("Run data, test results and recorded compiler/SDK identities are always included. Environment files are run working directories; installed compilers, host OS and system variables are not copied. Raw logs and files may contain secrets."),'muted'));
 let createRequest=null;
 const create=button(uiMessage("Create archive"),async()=>{
  if(!recipient.reportValidity())return;
  createRequest??={project_id,run_id:select.value,...(encryptionEnabled&&recipient.value.trim()?{encryption_recipient:recipient.value.trim()}:{}),request_id:crypto.randomUUID(),...Object.fromEntries(Object.entries(inputs).map(([k,v])=>[k,v.checked])),actor:'user',reason:'Manual run archival from Web UI'};
  const result=await api('archives',createRequest);createRequest=null;notice(uiMessage("Archive {0} · {1}",{"0":result.archive_id.slice(-8),"1":uiStatus(result.state)}));await refresh();
 });create.className='primary';options.append(create);recipient.oninput=()=>{createRequest=null;recipient.setCustomValidity(recipient.value.trim()&&!/^age1[a-z0-9]+$/.test(recipient.value.trim())?'Use an age1 public recipient. Do not enter a private identity.':'');};function syncCreate(){create.disabled=!select.value;}select.onchange=()=>{createRequest=null;syncCreate();};for(const input of Object.values(inputs))input.onchange=()=>{createRequest=null;};syncCreate();
 function archiveRow(item){
  const row=el('article',undefined,'archive-row');row.dataset.archiveId=item.archive_id;row.dataset.state=item.state;row.append(el('strong',uiMessage("Run {0} · {1}",{"0":item.run_id.slice(-8),"1":uiFormatted(new Date(item.created_at),"toLocaleString")})),el('span',uiStatus(item.state),'badge '+(item.state==='ready'?'succeeded':item.state==='failed'?'failed':item.state==='expired'?'skipped':'running')));
   if(item.encryption)row.append(el('p','Encrypted · age','muted'));
   if(item.imported)row.append(el('p',`Imported · origin project ${item.origin_project_id}`,'muted'));
   if(retentionEnabled&&item.state==='ready'){const pin=button(item.pinned?'Unpin archive':'Pin archive',async()=>{const result=await api('archive/pin',{project_id,archive_id:item.archive_id,pinned:!item.pinned,expected_revision:item.pin_revision||0,request_id:crypto.randomUUID(),actor:'user',reason:'Change archive retention pin from Web UI'});row.replaceWith(archiveRow({...item,pinned:result.pinned,pin_revision:result.revision}));if(!executeRequest){checkedPlan=null;cleanupCursor=null;erase.disabled=true;previewCleanup.disabled=false;review.replaceChildren();}});pin.setAttribute('aria-pressed',String(!!item.pinned));row.append(pin);}
   if(item.error)row.append(el('p',item.error,'error'));
   if(item.summary){row.append(el('p',uiMessage("{0} entries · {1} compressed bytes",{"0":uiFormatted(item.summary.entries,"toLocaleString"),"1":uiFormatted(item.file_bytes,"toLocaleString")}),'muted'));
    const missing=item.summary.missing||[];if(missing.length){const details=el('details');details.append(el('summary',uiMessage("{0} paths were already unavailable",{"0":missing.length})));for(const path of missing)details.append(el('p',path));row.append(details);}
    const digest=el('code',item.sha256);uiAttribute(digest,"title",uiMessage("Archive SHA256"));row.append(digest);
   }
   if(item.state==='ready'){const controls=el('div',undefined,'actions'),id={project_id,archive_id:item.archive_id};const download=el('a',uiMessage("Download"),'archive-download');download.href='/api/v1/archive/download?'+query(id);download.download=`preflight-${item.archive_id}.tar.zst${item.encryption?'.age':''}`;
    const identity=item.encryption?identityChoice('Decryption identity',true):null;
    if(identity)row.append(identity.label);
    const unlocked=()=>({...id,...(identity?.select.value?{identity_file_env:identity.select.value}:{})});
    controls.append(download,button(uiMessage("Verify"),async()=>{const result=await api('archive/verify',unlocked());notice(uiMessage("Verified {0} entries.",{"0":result.entries}));}),button(uiMessage("Restore files"),async()=>{const result=await api('archive/restore',{...unlocked(),request_id:crypto.randomUUID(),actor:'user',reason:'Restore archived files into isolated directory'});const path=el('div',undefined,'copy-field'),pre=el('pre',result.path),copy=button('⧉',async()=>{await navigator.clipboard.writeText(result.path);notice(uiMessage("Copied."));});copy.className='field-copy';uiAttribute(copy,'aria-label',uiMessage("Copy restored directory"));path.append(pre,copy);row.append(path,el('p',uiMessage("Restored to a separate directory. Live project and database were not changed."),'muted'));notice(uiMessage("Archive files restored."));}));if(identity){const sync=()=>controls.querySelectorAll('button').forEach(b=>{b.disabled=!identity.select.value;b.title=identity.select.value?'':'Select a configured server identity to decrypt.';});identity.select.onchange=sync;sync();}row.append(controls);
   }
  if(['queued','creating'].includes(item.state))pending.set(item.archive_id,row);else pending.delete(item.archive_id);
  return row;
 }
 function schedulePoll(){if(!closed&&pending.size)timer=setTimeout(()=>poll(),2000);}
 async function poll(){
  const request=generation;
  try{
   for(const [archive_id,row]of [...pending]){
    const item=await api('archive?'+query({project_id,archive_id}));
    if(closed||request!==generation||!row.isConnected)return;
    if(item.state!==row.dataset.state)row.replaceWith(archiveRow(item));
   }
   schedulePoll();
  }catch(error){
   if(closed||request!==generation)return;
   const warning=el('div',undefined,'archive-sync-error');warning.setAttribute('role','alert');
   const details=el('details');details.append(el('summary',uiMessage("Error details")),el('pre',error.message));
   warning.append(el('p',uiMessage("Cannot update archive status. Check the connection and retry.")),button(uiMessage("Retry archive status"),async()=>{const restoreFocus=warning.contains(document.activeElement);warning.remove();await poll();if(restoreFocus&&(document.activeElement===document.body||document.activeElement===dialog))(list.querySelector('button')||heading.querySelector('button')).focus();}),details);list.prepend(warning);
  }
 }
 async function refresh(append=false){
  clearTimeout(timer);const request=++generation;
  const data=await api('archives?'+query({project_id,pipeline_key,run_id,before:append?next:null}));if(closed||request!==generation)return;
  if(!append){pending.clear();list.replaceChildren();}next=data.next_cursor;
  if(!data.items.length&&!append)list.append(el('p',uiMessage("No archives yet."),'empty'));
  for(const item of data.items)list.append(archiveRow(item));
  if(next){const more=button(uiMessage("Load more archives"),async()=>{await refresh(true);more.remove();});list.append(more);}
  schedulePoll();
 }
 list.append(el('p',uiMessage("Loading archives…"),'loading-indicator'));await supported;await Promise.all([loadRuns(),refresh()]);
}
