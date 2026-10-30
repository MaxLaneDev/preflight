import {modalClose,closeDialog} from '/interactions.mjs';
import {msg as uiMessage,attr as uiAttribute,uiNode,uiError,formatted as uiFormatted} from '/localization.mjs';
import {openArchives} from '/archives.mjs';
// Maintenance is scoped before preview; applying never widens the approved plan.
export function installManagement({api,el,button,query,notice,onChanged,canRetention}){
 async function open({project_id,project_name,pipeline_key,pipeline_name,run_id}={}){
  if(!project_id)throw uiError(uiMessage("Choose a workspace first."));
  const dialog=el('dialog',undefined,'management-dialog');uiAttribute(dialog,'aria-label',uiMessage("Manage workspace data"));
  const head=el('div',undefined,'heading');head.append(el('h2',(run_id?uiMessage("Manage run"):(pipeline_key?uiMessage("Manage pipeline"):uiMessage("Manage workspace")))),modalClose(button(uiMessage("Close"),()=>closeDialog(dialog))));
  const context=el('p',(project_name || project_id),'muted'),scope=el('p',(run_id?uiMessage("Run · {0}",{"0":run_id}):(pipeline_key?uiMessage("Pipeline · {0}",{"0":pipeline_name||pipeline_key}):uiMessage("All pipelines in this workspace"))),'maintenance-scope');
  const actions=el('div',undefined,'maintenance-options'),body=el('section');body.setAttribute('aria-live','polite');
  const identity=el('div',undefined,'maintenance-identity');identity.append(context,scope);
  dialog.append(head,identity,actions,body);dialog.addEventListener('close',()=>dialog.remove());document.body.append(dialog);dialog.showModal();
  const retained=el('p',uiMessage("Items in Trash can be restored. File cleanup is separate."),'muted');dialog.append(retained);
  async function preview(action,before,append=Boolean(before)){
   if(!append)body.replaceChildren(el('p',uiMessage("Preparing preview…"),'loading-indicator'));
   try{
    const plan=await api('management/preview',{project_id,action,target_id:action==='delete_run'?run_id:action==='delete_pipeline'?pipeline_key:undefined,pipeline_key:action==='artifacts'?pipeline_key:undefined,before});
    render(plan,action,append);
   }catch(error){if(!append)body.replaceChildren(el('p',error.message,'error'));throw error;}
  }
  function retention(){
   body.replaceChildren(el('h3','Retention cleanup'));
   const form=el('form'),kind=el('select'),cutoff=el('input'),ack=el('input');
   kind.setAttribute('aria-label','Payload category');
   for(const [value,label]of [['logs','Log payloads'],['source_snapshots','Source snapshots'],['cache_artifacts','Rebuildable cache outputs']])kind.append(new Option(label,value));
   cutoff.type='datetime-local';cutoff.step='1';cutoff.required=true;cutoff.setAttribute('aria-label','Completed before');
   const date=new Date(Date.now()-30*86400000);cutoff.value=new Date(date.getTime()-date.getTimezoneOffset()*60000).toISOString().slice(0,19);
   ack.type='checkbox';ack.required=true;const label=el('label','I understand payload removal cannot be undone.');label.prepend(ack);
   const explain=el('p','Only old successful runs qualify. Failures, pins, modules, baselines and active operations are protected. History is retained. Log expiry reuses SQLite pages; it does not shrink the database file.','muted');
   const submit=el('button','Preview cleanup','primary');submit.type='submit';
   form.append(el('label','Category'),kind,el('label','Completed before'),cutoff,label,explain,submit);body.append(form);
   form.addEventListener('submit',async event=>{event.preventDefault();if(!form.reportValidity())return;
    const time=new Date(cutoff.value).getTime();if(!Number.isFinite(time)||time>Date.now()){notice('Choose a past date and time.','error');return;}
    submit.disabled=true;try{
     const request={project_id,pipeline_key,run_id,acknowledge_loss:ack.checked,finished_before:time};
     if(kind.value!=='cache_artifacts')request.category=kind.value;
     const category=kind.value;
     async function load(cursor){
      const args={...request};if(cursor){if(category==='cache_artifacts')args.offset=Number(cursor);else args.before=cursor;}
      const plan=await api(category==='cache_artifacts'?'storage/cache/preview':'storage/evidence/preview',args);
      plan.execute_tool='storage_gc_execute';plan.effect=explain.textContent;plan.nextPage=()=>load(plan.next_cursor);render(plan,'retention',Boolean(cursor));
     }
     await load();
    }catch(error){submit.disabled=false;notice(error.message,'error');}
   });
  }
  function render(plan,action,append=false){
   if(!append)body.replaceChildren();const batch=el('section',undefined,'maintenance-preview-batch');body.append(batch);
   batch.replaceChildren(el('h3',uiMessage("Review changes")));
   const entries=plan.entries||[],eligible=entries.filter(row=>row.eligible!==false);
   batch.append(el('p',(action==='artifacts'?uiMessage("Only eligible output files will be deleted. Protected files are skipped; test and log history remains."):plan.effect),'muted'));
   if(plan.estimated_file_bytes!==undefined)batch.append(el('p',uiMessage("{0} file bytes eligible",{"0":uiFormatted(plan.estimated_file_bytes,"toLocaleString")})));
   const list=el('div',undefined,'maintenance-preview');
   for(const row of entries){const line=el('article');line.append(el('strong',((row.name || (row.run_id && `${row.run_id.slice(-8)}${row.stage_id?' / '+row.stage_id:''}`)) || row.target_id)),el('span',(row.reason || (row.eligible===false?uiMessage("Protected"):(row.kind || uiMessage("Eligible")))),'muted'));list.append(line);}
   if(!entries.length)list.append(el('p',uiMessage("Nothing to change."),'empty'));batch.append(list);
   const controls=el('div',undefined,'actions');
   const request={project_id,plan_id:plan.plan_id,request_id:crypto.randomUUID(),actor:'user',reason:'Manual scoped maintenance from Web UI'};
   const apply=button((action==='retention'?'Expire approved payloads':action==='artifacts'?uiMessage("Delete eligible files"):(action==='clear_queue'?uiMessage("Clear listed pending tasks"):uiMessage("Move to Trash"))),async()=>{
    apply.disabled=true;actions.inert=true;
    try{
     const result=await api(plan.execute_tool==='storage_gc_execute'?'storage-gc-execute':'management/execute',request);
     const failed=(result.items||[]).filter(item=>item.state==='failed');
     batch.replaceChildren(el('h3',(failed.length?uiMessage("Completed with errors"):uiMessage("Completed"))),el('p',uiMessage("Only items in this preview were processed.")));
     if(result.expired_log_payload_bytes)batch.append(el('p',`${result.expired_log_payload_bytes} logical log bytes expired; database pages are reusable.`));
     if(result.deleted_file_bytes!==undefined)batch.append(el('p',uiMessage("{0} file bytes deleted",{"0":uiFormatted(result.deleted_file_bytes,"toLocaleString")})));
     const items=result.items||result.entries||[];
     for(const item of items)batch.append(el('p',`${item.target_id||item.run_id||item.ordinal||''} · ${item.state||item.status||''}${item.reason?' · '+item.reason:''}`));
     if(result.plan_id&&plan.execute_tool==='storage_gc_execute'){const status=await api('storage-gc-status?'+query({project_id,plan_id:plan.plan_id}));batch.append(el('p',uiMessage("Cleanup · {0}",{"0":((status.state || status.status) || uiMessage("processed"))})));}
     if(plan.has_more||plan.next_cursor){const next=button(uiMessage("Review next batch"),async()=>{await (action==='retention'?plan.nextPage():preview(action,plan.next_cursor||undefined,true));next.remove();});batch.append(next);}
     await onChanged({project_id,action,target_id:run_id||pipeline_key});notice((failed.length?uiError(uiMessage("{0} cleanup items failed. Review the result.",{"0":failed.length})):uiMessage("Maintenance completed.")));
    }finally{actions.inert=false;if(apply.isConnected)apply.disabled=false;}
   });apply.className='danger';apply.disabled=!eligible.length;controls.append(apply);
   if(plan.next_cursor){const next=button(uiMessage("Next preview page"),async()=>{await (action==='retention'?plan.nextPage():preview(action,plan.next_cursor,true));next.remove();});controls.append(next);}batch.append(controls);
  }
  const actionButton=(label,callback,style,paths)=>{const control=button(uiMessage(label),callback);control.classList.add('maintenance-'+style);const icon=document.createElementNS('http://www.w3.org/2000/svg','svg');icon.setAttribute('viewBox','0 0 24 24');icon.setAttribute('fill','none');icon.setAttribute('stroke','currentColor');icon.setAttribute('stroke-width','1.7');icon.setAttribute('stroke-linecap','round');icon.setAttribute('stroke-linejoin','round');icon.setAttribute('aria-hidden','true');icon.innerHTML=paths;control.prepend(icon);return control;};
  const trashIcon='<path d="M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7m4-7v7"/>';
  if(canRetention?.())actions.append(actionButton('Retention cleanup',retention,'cleanup','<path d="M4 4h16v5H4zM6 9v11h12V9M9 13h6m-6 3h6"/>'));
  if(run_id)actions.append(actionButton('Move this run to Trash',()=>preview('delete_run'),'delete',trashIcon));
  else{
  actions.append(actionButton('Clean artifact files',()=>preview('artifacts'),'cleanup','<path d="m16 3 5 5-9 9-5-5zM7 12l-4 9h9l4-4M6 16l3 5"/>'));
   if(pipeline_key)actions.append(actionButton('Move this pipeline to Trash',()=>preview('delete_pipeline'),'delete',trashIcon));
   else actions.append(actionButton('Clear pending queue',()=>preview('clear_queue'),'queue','<path d="M3 6h18M3 12h10M3 18h10m4-5 5 5m0-5-5 5"/>'));
  }
  actions.append(actionButton('Archives',()=>openArchives({api,el,button,query,notice,project_id,pipeline_key,run_id}),'archive','<rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v13h14V8M9 12h6"/>'));
  const trashButton=actionButton('Trash',()=>loadTrash(),'trash','<path d="M3 12a9 9 0 1 0 3-7M3 3v6h6"/><path d="M12 7v5l3 2"/>');actions.append(trashButton);
  async function loadTrash(after,append=false){
   if(!append)body.replaceChildren(el('h3',uiMessage("Trash")));
   const data=await api('management/trash?'+query({project_id,after}));
   if(!data.items.length&&!append)body.append(el('p',uiMessage("Trash is empty."),'empty'));
   for(const row of data.items){const item=el('div',undefined,'maintenance-trash-row');item.append(el('span',uiMessage("{0} · {1} · {2}",{"0":(row.kind==='pipeline'?uiMessage("Pipeline"):uiMessage("Run")),"1":row.kind==='pipeline'?row.target_id.replace(/^saved:/,''):row.target_id.slice(-8),"2":uiFormatted(new Date(row.deleted_at),"toLocaleString")})));item.append(button(uiMessage("Restore"),async()=>{await api('management/restore',{project_id,kind:row.kind,target_id:row.target_id,actor:'user',reason:'Restore from workspace Trash'});await onChanged({project_id,action:'restore',kind:row.kind,target_id:row.target_id});await loadTrash();trashButton.focus();notice(uiMessage("Restored."));}));body.append(item);}
   if(data.next_cursor){const more=button(uiMessage("Load more"),async()=>{await loadTrash(data.next_cursor,true);more.remove();});body.append(more);}
  }
 }
 return {open};
}
