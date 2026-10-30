import {mountVirtualList} from './virtual-list.mjs';
// Logs are evidence: format the view without changing the persisted payload or outcome.
export class ConsoleLines {
 constructor({maxCharacters=1024*1024,maxLines=5000}={}){
  this.maxCharacters=maxCharacters;this.maxLines=maxLines;this.lines=[];this.pending=new Map();this.discardedPending=new Set();this.characters=0;this.serial=0;this.dropped=false;
 }
 append(events){
  const changed=new Set(),removed=[];
  for(const event of events){
   const stream=event.stream||'stdout',stage=event.stage_id||'',key=JSON.stringify([stage,stream]);
   let start=0;
   while(start<event.text.length){
    let line=this.pending.get(key);
    if(!line){const previous=this.lines.at(-1);line={id:++this.serial,key:String(event.id??this.serial)+':'+start,stage,stream,timestamp:event.timestamp,text:'',complete:false,truncated:this.discardedPending.has(key),newGroup:!previous||previous.stage!==stage||previous.stream!==stream};this.lines.push(line);this.pending.set(key,line);}
    const end=event.text.indexOf('\n',start),part=event.text.slice(start,end<0?undefined:end+1);
    line.text+=part;this.characters+=part.length;changed.add(line);
    if(end<0)break;
    line.complete=true;this.pending.delete(key);this.discardedPending.delete(key);start=end+1;
   }
  }
  while(this.lines.length>1&&(this.characters>this.maxCharacters||this.lines.length>this.maxLines)){
   const line=this.lines.shift();this.characters-=line.text.length;changed.delete(line);removed.push(line.id);
   const key=JSON.stringify([line.stage,line.stream]);if(this.pending.get(key)===line){this.pending.delete(key);this.discardedPending.add(key);}this.dropped=true;
  }
  const first=this.lines[0];
  if(first&&first.text.length>this.maxCharacters){this.characters-=first.text.length-this.maxCharacters;first.text=first.text.slice(-this.maxCharacters);first.truncated=true;changed.add(first);this.dropped=true;}
  return {changed:[...changed],removed,dropped:this.dropped};
 }
 finish(){const changed=[...this.pending.values()];for(const line of changed)line.complete=true;this.pending.clear();return {changed,removed:[],dropped:this.dropped};}
 rawText(){return this.lines.map(line=>'['+line.stage+'/'+line.stream+'] '+line.text).join('');}
}

export function describeLogLine(line){
 if(!line.complete||line.truncated||line.text.length>256*1024)return null;
 const text=line.text.trim();if(!text.startsWith('{')&&!text.startsWith('['))return null;
 let value;try{value=JSON.parse(text);}catch{return null;}
 if(!value||typeof value!=='object')return null;
 const string=v=>typeof v==='string'?v:'';
 const title=string(value.label)||string(value.name)||string(value.event)||string(value.message)||string(value.msg)||(Array.isArray(value)?'JSON array':'JSON record');
 const meta=[];
 for(const key of ['level','opt','temperature'])if(string(value[key]))meta.push(value[key]);
 if(typeof value.passed==='boolean')meta.push(value.passed?'check passed':'check failed');
 if(Number.isInteger(value.exitCode))meta.push('exit '+value.exitCode);
 if(string(value.signal))meta.push('signal '+value.signal);
 if(Number.isFinite(value.elapsedMs)&&value.elapsedMs>=0)meta.push(value.elapsedMs+' ms');
 const messages=[];
 for(const key of ['message','msg','error'])if(string(value[key])&&value[key]!==title)messages.push(value[key]);
 for(const key of ['diagnostics','testLines'])if(Array.isArray(value[key]))for(const item of value[key])if(typeof item==='string')messages.push(item);
 const limited=messages.slice(0,200);
 if(messages.length>limited.length)limited.push('Additional output is available in Details.');
 return {value,title,meta:meta.join(' · '),messages:limited,tone:typeof value.passed==='boolean'?(value.passed?'passed':'failed'):null};
}

export function mountConsole(container,{copyCode,virtual=false}={}){
 let model=new ConsoleLines(),raw=false;const nodes=new Map();
 const node=(tag,text,cls)=>{const element=container.ownerDocument.createElement(tag);if(text!==undefined)element.textContent=text;if(cls)element.className=cls;return element;};
 function draw(line){
  let row=nodes.get(line.id);
  if(!row){row=node('div',undefined,'console-entry');row.dataset.newGroup=String(line.newGroup);nodes.set(line.id,row);if(!virtual)container.append(row);}
  const description=raw?null:describeLogLine(line);row.classList.toggle('console-structured',Boolean(description));
  row.classList.toggle('console-lifecycle',line.stream==='lifecycle');row.replaceChildren();
  const header=node('div',undefined,'console-meta'),date=new Date(line.timestamp);
  if(Number.isFinite(date.getTime()))header.append(node('time',date.toLocaleTimeString('en-GB',{hour12:false})));
  header.append(node('span',line.stage+' / '+line.stream));row.append(header);
  if(!description){row.append(node('span',line.text,line.stream==='stderr'?'console-message log-stderr':'console-message'));return row;}
  const heading=node('div',undefined,'console-record-heading');heading.append(node('strong',description.title));
  if(description.meta)heading.append(node('span',description.meta,'console-record-meta'));
  if(description.tone)heading.dataset.check=description.tone;
  row.append(heading);
  for(const message of description.messages)row.append(node('div',message,'console-message'));
  const details=node('details',undefined,'console-json-details');details.append(node('summary','Details'));
  const formatted=JSON.stringify(description.value,null,2);
  details.append(copyCode?copyCode(formatted):node('pre',formatted,'console-json'));
  row.append(details);return row;
 }
 const window=virtual?mountVirtualList(container,{key:line=>line.key,estimate:30,render:line=>{nodes.clear();return draw(line);}}):null;
 function apply(change){
  if(window){window.setItems(model.lines);return change.dropped;}
  for(const id of change.removed){nodes.get(id)?.remove();nodes.delete(id);}
  for(const line of change.changed)draw(line);
  return change.dropped;
 }
 return {
  append:events=>apply(model.append(events)),finish:()=>apply(model.finish()),
  replace:(events,{end=false,complete=false}={})=>{model=new ConsoleLines({maxCharacters:4*1024*1024,maxLines:100000});model.append(events);if(complete)model.finish();nodes.clear();window.setItems(model.lines,{end,redraw:true});return model.lines.length;},
  end:()=>window?.end(),
  destroy:()=>window?.destroy(),
  setRaw:value=>{raw=value;nodes.clear();if(window){window.setItems(model.lines,{redraw:true});return;}container.replaceChildren();for(const line of model.lines)draw(line);},
  rawText:()=>model.rawText()
 };
}
