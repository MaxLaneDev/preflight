import {bindPageChoices} from './virtual-list.mjs';
import {modalClose,closeDialog} from '/interactions.mjs';
export const diagnosticLabels={new:'New',fixed:'Fixed',recurring:'Recurring',observed:'Observed',not_observed_in_baseline_window:'Not seen in baseline window',not_observed_in_current_window:'Not seen in current window'};
export async function openDiagnostics({project,runId,baselineId,stageId,api,el,button,code,copy,runLabel,onEvidence,isCurrent=()=>true}){
 const dialog=el('dialog',undefined,'diagnostics-dialog');dialog.setAttribute('aria-label','Compiler diagnostics');
 const heading=el('div',undefined,'heading');heading.append(el('h2','Compiler diagnostics'),modalClose(button('Close',()=>closeDialog(dialog))));
 const filters=el('div',undefined,'diagnostic-filters');
 const select=(label)=>{const wrapper=el('label',label),input=el('select');input.setAttribute('aria-label',label);wrapper.append(input);filters.append(wrapper);return input;};
 const run=select('Diagnostic run'),baseline=select('Compare diagnostics with'),stage=select('Diagnostic stage');baseline.append(new Option('No baseline',''));stage.append(new Option('All stages',''));
 const status=el('p','Loading runs…','muted');status.setAttribute('role','status');
 const list=el('div'),actions=el('div',undefined,'actions');let cursor=null,offset=null,scanBytes=1048576,scanEvents=4096,generation=0,choiceGeneration=0,timer,lastReport;
 const older=button('Older runs',()=>loadRuns()),more=button('More diagnostics',()=>load(false)),expand=button('Continue analysis',()=>{scanBytes=Math.min(4194304,scanBytes*2);scanEvents=Math.min(10000,scanEvents*2);load();}),copyReport=button('Copy report',()=>copy({...lastReport,groups:[...list.children].map(n=>n.diagnostic)}));
 bindPageChoices(older,[run,baseline]);older.disabled=copyReport.disabled=true;more.hidden=expand.hidden=true;actions.append(older,copyReport);dialog.append(heading,filters,actions,status,list,more,expand);
 const closeOnRoute=()=>closeDialog(dialog);
 dialog.addEventListener('close',()=>{generation++;clearTimeout(timer);window.removeEventListener('popstate',closeOnRoute);dialog.remove();});window.addEventListener('popstate',closeOnRoute);
 document.body.append(dialog);dialog.showModal();
 const params=(extra={})=>new URLSearchParams(Object.entries({project_id:project,...extra}).filter(([,v])=>v!==null&&v!==undefined&&v!=='')).toString();
 const appendRun=(record)=>{const value=record.run_id||record.id;if(![...run.options].some(o=>o.value===value)){run.append(new Option(String(runLabel({...record,run_id:value})),value));baseline.append(new Option(String(runLabel({...record,run_id:value})),value));}};
 async function loadRuns(){
  older.dataset.pageError='';older.disabled=true;
  try{
   const page=await api('run-history?'+params({before:cursor,include_experiments:true,limit:25}));if(!dialog.open||!isCurrent())return;
   for(const item of page.runs)appendRun(item);cursor=page.next_cursor;older.hidden=!cursor;
   if(runId){if(![...run.options].some(o=>o.value===runId)){const selected=await api('runs/'+encodeURIComponent(runId));if(!dialog.open||!isCurrent())return;if(selected.project_id!==project)throw Error('Run project mismatch');appendRun(selected);}run.value=runId;runId=null;}
   if(baselineId){if(![...baseline.options].some(o=>o.value===baselineId)){const data=await api('runs/'+encodeURIComponent(baselineId));if(!dialog.open||!isCurrent())return;if(data.project_id!==project)throw Error('Baseline project mismatch');appendRun(data);}baseline.value=baselineId;baselineId=null;}
   if(!run.options.length){status.textContent='No runs in this project.';return;}
   if(!lastReport){await loadStages();await load();}
  }catch(error){if(dialog.open){older.dataset.pageError=error.message;status.textContent=error.message;status.setAttribute('role','alert');}}
  finally{older.disabled=false;}
 }
 async function loadStages(){
  const selected=run.value,request=++choiceGeneration;if(!selected)return;
  const data=await api('runs/'+encodeURIComponent(selected));if(!dialog.open||run.value!==selected||request!==choiceGeneration)return;
  stage.replaceChildren(new Option('All stages',''));for(const item of data.stages||[])stage.append(new Option(item.definition.id,item.definition.id));
  stage.value=[...stage.options].some(o=>o.value===stageId)?stageId:'';stageId=stage.value;
 }
 async function load(reset=true){
  clearTimeout(timer);more.dataset.pageError='';const request=++generation;if(!run.value)return;
  if(reset){offset=null;list.replaceChildren();}status.textContent='Reading diagnostics…';status.setAttribute('role','status');more.disabled=true;copyReport.disabled=true;
  try{
   const report=await api('diagnostics?'+params({run_id:run.value,baseline_id:baseline.value,stage_id:stage.value,offset:reset?0:offset,scan_bytes:scanBytes,scan_events:scanEvents,limit:20,max_bytes:16384}));
   if(!dialog.open||!isCurrent()||request!==generation)return;
   lastReport=report;if(reset)list.replaceChildren();
   const currentComplete=report.current_scan.complete,baselineComplete=!report.baseline_scan||report.baseline_scan.complete;
   status.textContent=Object.entries(report.group_counts).map(([key,count])=>`${diagnosticLabels[key]||key}: ${count}`).join(' · ')||'No recognized compiler diagnostics.';
   if(!currentComplete||!baselineComplete)status.textContent+=' · Analysis incomplete: new/fixed classifications are unavailable.';else if(baseline.value&&!report.comparison_complete)status.textContent+=' · Missing or unexecuted stages are not classified as fixed.';if(report.current_scan.omitted_lines||report.baseline_scan?.omitted_lines)status.textContent+=' Some lines exceeded analysis limits or were unavailable; inspect their original log evidence.';
   for(const group of report.groups){
    const card=el('article',undefined,'card diagnostic-card');card.diagnostic=group;card.dataset.state=group.state;card.dataset.fingerprint=group.fingerprint;
    const top=el('div',undefined,'heading');top.append(el('strong',`${group.stage_id} · ${group.severity}${group.code?' ['+group.code+']':''}`),el('span',diagnosticLabels[group.state]||group.state,'badge'));card.append(top,code(group.message));for(const text of group.details||[])card.append(el('p',text,'muted'));
    if(group.file)card.append(el('p',`${group.file}${group.line?':'+group.line:''}${group.column?':'+group.column:''}`,'diagnostic-location'));
    const counts=group.state==='fixed'?`Baseline occurrences: ${group.baseline_occurrences}`:`Occurrences: ${group.occurrences}${group.baseline_occurrences!==null?' · Baseline: '+group.baseline_occurrences:''}`;card.append(el('p',counts,'muted'));
    const row=el('div',undefined,'actions');for(const [n,id]of group.evidence_ids.entries())row.append(button(n?'Context '+(n+1):'Log context',()=>onEvidence({evidence_id:id,stage_id:group.stage_id,stream:'diagnostic'})));row.append(button('Copy diagnostic',()=>copy(group)));card.append(row);const existing=[...list.children].find(n=>n.dataset.fingerprint===group.fingerprint);if(existing)existing.replaceWith(card);else list.append(card);
   }
   offset=report.next_offset;more.hidden=offset===null;expand.hidden=report.current_scan.drained&&(!report.baseline_scan||report.baseline_scan.drained);copyReport.disabled=false;
   const terminal=new Set(['succeeded','failed','cancelled','timed_out','infrastructure_error','blocked','skipped']);
   if(!terminal.has(report.run_status)||(report.baseline_status&&!terminal.has(report.baseline_status)))timer=setTimeout(()=>load(),3000);
  }catch(error){if(dialog.open&&request===generation){more.dataset.pageError=error.message;status.textContent=error.message;status.setAttribute('role','alert');}}
  finally{if(request===generation)more.disabled=false;}
 }
 run.onchange=async()=>{scanBytes=1048576;scanEvents=4096;try{await loadStages();await load();}catch(error){status.textContent=error.message;status.setAttribute('role','alert');}};baseline.onchange=stage.onchange=()=>{stageId=stage.value;scanBytes=1048576;scanEvents=4096;load();};more.onclick=()=>load(false);
 await loadRuns();
 return dialog;
}
