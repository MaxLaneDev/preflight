import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';

const app=readFileSync(new URL('../src/app.js',import.meta.url),'utf8');
const extract=(start,end)=>app.slice(app.indexOf(start),app.indexOf(end,app.indexOf(start)));

test('focused search fields do not stop polling and SSE bypasses catalog throttling',async()=>{
 const calls=[],nodes={explorer:{hidden:false},'test-dialog':{},editor:{},'schedule-dialog':{},'sync-status':{}};
 const context={state:{section:'pipelines',project:'project'},document:{hidden:false,activeElement:{matches:()=>true},querySelector:()=>null},settingsActive:false,polling:false,$:id=>nodes[id],refreshCatalog:async value=>calls.push(value),uiText(){},uiAttribute(){},uiMessage:x=>x,uiFormatted(){},Date};
 const poll=runInNewContext('('+extract('async function pollCurrentPage(',"setInterval(()=>{if(!liveConnected")+')',context);
 await poll({eventDriven:true});assert.deepEqual(calls,[true]);
 await poll();assert.deepEqual(calls,[true,false]);
});

test('SSE hints refresh catalog state even when completion and log cursors are unchanged',async()=>{
 const source=new EventTarget();let refreshed=0;
 const context={state:{project:'p',run:null,section:'pipelines'},document:{hidden:false,querySelector:()=>null},liveKey:null,liveSource:null,liveConnected:false,lastLiveCompletion:null,sharedLiveSource:()=>source,api:async()=>({completion_cursor:0,log_cursor:0,run:null}),pollCurrentPage:async options=>{assert.equal(options.eventDriven,true);refreshed++;},query:()=>'',uiAttribute(){},$:()=>({}),MessageEvent};
 runInNewContext('('+extract('function syncLive()',"try{const connection=")+')',context)();
 for(let n=0;n<2;n++){source.dispatchEvent(new MessageEvent('change',{data:'{"hint":true}'}));await new Promise(setImmediate);}
 assert.equal(refreshed,2);
});

test('elapsed clocks advance without network activity and finished durations stay fixed',async()=>{
 const events=new EventTarget();globalThis.document=Object.assign(new EventTarget(),{hidden:false});globalThis.addEventListener=events.addEventListener.bind(events);
 try{
  const {elapsedText,elapsedNode,tickElapsed}=await import('../src/live-client.mjs');
  const node={dataset:{}};elapsedNode(node,1000,null);
  tickElapsed({querySelectorAll:()=>[node]},61000);assert.equal(node.textContent,'1 min 0 s elapsed · updating');
  assert.equal(elapsedText(1000,4600,90000),'3.6 s elapsed');
  elapsedNode(node,1000,4600);assert.equal(node.dataset.elapsedEnd,'4600');
  document.hidden=true;tickElapsed({querySelectorAll:()=>{throw Error('Hidden page must not tick');}},90000);
 }finally{delete globalThis.document;delete globalThis.addEventListener;}
});

test('queue state refreshes after a transition while its search control keeps focus',async()=>{
 const source=readFileSync(new URL('../src/executions.mjs',import.meta.url),'utf8');
 const body=source.slice(source.indexOf('let summaryLoading=false'),source.indexOf(' function connect()'));
 let state='running',loads=0;const context={document:{hidden:false},panel:{contains:()=>true},visible:true,since:0,signature:'',api:async()=>({state}),query:()=>'',load:()=>{loads++;},uiText(){},uiAttribute(){},counts:{},uiMessage:x=>x};
 context.renderSummary=data=>{context.signature=JSON.stringify(data);};
 const refresh=runInNewContext(body+';refreshSummary',context);
 await refresh();state='succeeded';await refresh();assert.equal(loads,2);
});

test('hour durations and compact stage clocks advance locally',async()=>{
 globalThis.document=Object.assign(new EventTarget(),{hidden:false});globalThis.addEventListener=()=>{};
 try{
  const {durationText,elapsedNode,tickElapsed}=await import('../src/live-client.mjs');
  assert.equal(durationText(3813000),'1 h 3 min 33 s');
  assert.equal(durationText(86400000),'24 h 0 min 0 s');
  const stage={dataset:{}};elapsedNode(stage,1000,null,true);
  tickElapsed({querySelectorAll:()=>[stage]},3814000);assert.equal(stage.textContent,'1 h 3 min 33 s');
  elapsedNode(stage,1000,4000,true);assert.equal(stage.textContent,'3.0 s');assert.equal(stage.dataset.elapsedEnd,'4000');
 }finally{delete globalThis.document;delete globalThis.addEventListener;}
});

test('completion notifications drain event pages without skipping events or duplicating selected-run notices',async()=>{
 const source=new EventTarget(),notices=[];let snapshot=0;
 const context={state:{project:'p',run:null,section:'pipelines'},document:{hidden:false,querySelector:()=>null},liveKey:null,liveSource:null,liveConnected:false,lastLiveCompletion:null,sharedLiveSource:()=>source,
  api:async url=>{if(url.startsWith('live/snapshot'))return {completion_cursor:snapshot++?105:0,log_cursor:0,run:null};const after=Number(new URLSearchParams(url.split('?')[1]).get('after')),end=Math.min(after+100,105);return {events:Array.from({length:end-after},(_,i)=>({run_id:'run-'+(after+i+1),status:'succeeded'})),next_cursor:end,has_more:end<105};},
  pollCurrentPage:async()=>{},query:value=>new URLSearchParams(value).toString(),uiAttribute(){},$:()=>({}),notice:text=>notices.push(text),uiError:x=>x,MessageEvent};
 runInNewContext('('+extract('function syncLive()',"try{const connection=")+')',context)();
 source.dispatchEvent(new MessageEvent('change',{data:'{"hint":true}'}));await new Promise(setImmediate);
 source.dispatchEvent(new MessageEvent('change',{data:'{"hint":true}'}));await new Promise(setImmediate);await new Promise(setImmediate);
 assert.equal(notices.length,105);assert.equal(new Set(notices).size,105);assert.equal(notices.at(-1),'Preflight: run run-105 completed — succeeded.');
});
