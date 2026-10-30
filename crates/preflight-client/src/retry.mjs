import {msg as uiMessage,attr as uiAttribute,uiError} from './localization.mjs';
import {modalClose,closeDialog} from './interactions.mjs';
export async function openRetry({api,el,button,notice,run,stageId,testId,testName,onStarted}){
 const stage=structuredClone(run.stages.find(s=>s.definition.id===stageId).definition),fixture=stage.fixtures?.suite.cases.find(c=>c.id===testId),single=Boolean(testId);
 const dialog=el('dialog',undefined,'retry-dialog'),heading=el('div',undefined,'heading'),title=single?uiMessage('Retry test'):uiMessage('Retry stage');uiAttribute(dialog,'aria-label',title);heading.append(el('h2',title),modalClose(button(uiMessage('Close'),()=>closeDialog(dialog))));dialog.append(heading,el('p',single?(testName||fixture?.id||run.stages.find(s=>s.definition.id===stageId).tests?.cases.find(c=>c.id===testId)?.name||testId):stageId),el('p',uiMessage('Uses current project files and the original successful dependency outputs. Other stages and whole-pipeline criteria are not rechecked.'),'muted'));
 let revision=0,key=crypto.randomUUID(),checked=null;const fields=el('div',undefined,'retry-fields'),preview=el('div',undefined,'retry-preview');
 const invalidate=()=>{revision++;checked=null;start.disabled=true;key=crypto.randomUUID();preview.replaceChildren();};
 function field(label,value,write,multiline=false){const wrap=el('label',label),input=el(multiline?'textarea':'input');input.value=value;if(multiline)input.rows=5;input.oninput=()=>{write(input.value);invalidate();};wrap.append(input);fields.append(wrap);return input;}
 let argsField=null,argsCommand=null,argsJson=false,expectationField=null;
 if(fixture){for(const[path,source]of Object.entries(fixture.files))field(path,source,value=>fixture.files[path]=value,true);expectationField=field(uiMessage('Expected results (JSON)'),JSON.stringify(fixture.expect,null,2),()=>{},true);}
 else{
  if(single){stage.testing??={kind:'custom',reports:[]};stage.testing.rerun??=structuredClone(stage.command);fields.append(el('p',uiMessage('Runner arguments must contain a whole ${test.name} or ${test.id} argument. The report must contain the selected test result.'),'muted'));}
  const command=single?stage.testing.rerun:stage.command;
  field(uiMessage('Program'),command.program,value=>command.program=value);
  if(!stage.fixtures){argsCommand=command;argsJson=command.args.some(value=>value===''||value.includes('\n'));argsField=field(argsJson?uiMessage('Arguments (JSON array)'):uiMessage('Arguments · one per line'),argsJson?JSON.stringify(command.args,null,2):command.args.join('\n'),()=>{},true);}
 }
 const reason=field(uiMessage('Reason'),uiMessage('Retry after changes'),()=>{});
 const selection=()=>{if(argsField){const args=argsJson?JSON.parse(argsField.value):(argsField.value===''?[]:argsField.value.split('\n'));if(!Array.isArray(args)||args.some(v=>typeof v!=='string'))throw uiError(uiMessage('Arguments must be a JSON array of strings.'));argsCommand.args=args;}if(expectationField)fixture.expect=JSON.parse(expectationField.value);return ({project_id:run.project_id,run_id:run.id,stage_id:stageId,test_id:testId||null,stage_override:stage,source:null});};
 const check=button(uiMessage('Preview retry'),async()=>{const version=revision,selected=structuredClone(selection()),result=await api('pipeline-retry/preview',selected);if(version!==revision||!dialog.open)return;checked=selected;preview.replaceChildren(el('strong',uiMessage('Only this stage will run')));for(const item of result.retry.reused_artifacts)preview.append(el('p',`${item.stage_id} / ${item.name} · ${item.sha256}`));if(!result.retry.reused_artifacts.length)preview.append(el('p',uiMessage('No new dependency executions. Existing module inputs keep their identities.'),'muted'));start.disabled=false;});
 const start=button(uiMessage('Start retry'),async()=>{if(!checked)return;const result=await api('pipeline-retry',{selection:checked,idempotency_key:key,actor:'user',reason:reason.value});await closeDialog(dialog);notice(uiMessage('Partial retry started.'));await onStarted(result);});start.className='primary';start.disabled=true;
 const actions=el('div',undefined,'actions');actions.append(check,start);dialog.append(fields,preview,actions);dialog.addEventListener('close',()=>dialog.remove());document.body.append(dialog);dialog.showModal();
}
