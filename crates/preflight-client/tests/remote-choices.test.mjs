import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';

const app=readFileSync(new URL('../src/app.js',import.meta.url),'utf8');
const source=app.slice(app.indexOf('function remoteChoices('),app.indexOf('const pipelineListName='));
const option=(textContent,value)=>({textContent,value,dataset:{}});

function choices(rows,value,pages){
 const listeners=new Map();let scheduled;
 const select={options:rows.map(row=>option(row.name,row.id)),value,dataset:{},isConnected:true,
  get selectedOptions(){return this.options.filter(o=>o.value===this.value);},
  replaceChildren(...options){this.options=options;this.value=options[0]?.value||'';},
  append(o){this.options.push(o);if(this.options.length===1)this.value=o.value;},
  addEventListener(name,callback){listeners.set(name,callback);}
 };
 const remoteChoices=runInNewContext('('+source+')',{
  AbortController,state:{project:value},syncSelects(){},uiOption:option,
  cloneChoice:o=>o?{...o,dataset:{...o.dataset}}:null,
  query:()=>'',api:async()=>({items:pages.shift(),next_cursor:null}),
  clearTimeout(){},setTimeout(callback){scheduled=callback;return 1;}
 });
 remoteChoices(select,'workspaces');
 return {select,async reload(){listeners.get('choices-search')({detail:''});scheduled();await new Promise(setImmediate);}};
}

test('opening the workspace selector preserves catalog order after changing selection',async()=>{
 const rows=[{id:'compiler',name:'Compiler'},{id:'core',name:'SDK core'},{id:'alloc',name:'SDK alloc'},{id:'platform',name:'SDK platform'}];
 const {select,reload}=choices(rows,'alloc',[rows,rows]);
 await reload();assert.deepEqual(select.options.map(o=>o.value),rows.map(r=>r.id));assert.equal(select.value,'alloc');
 select.value='platform';await reload();
 assert.deepEqual(select.options.map(o=>o.value),rows.map(r=>r.id));assert.equal(select.value,'platform');
});

test('a selected workspace outside the current page is retained without moving catalog rows',async()=>{
 const rows=[{id:'a',name:'A'},{id:'b',name:'B'},{id:'c',name:'C'}];
 const {select,reload}=choices(rows,'c',[rows.slice(0,2)]);
 await reload();assert.deepEqual(select.options.map(o=>o.value),['a','b','c']);assert.equal(select.value,'c');
});

test('an empty placeholder remains first and selected during remote loading',async()=>{
 const rows=[{id:'',name:'Choose a workspace…'}];
 const {select,reload}=choices(rows,'',[[{id:'a',name:'A'}]]);
 await reload();assert.deepEqual(select.options.map(o=>o.value),['','a']);assert.equal(select.value,'');
});
