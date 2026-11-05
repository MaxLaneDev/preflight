import test from 'node:test';
import assert from 'node:assert/strict';
import {isPageLabel} from '../src/virtual-list.mjs';
test('cursor controls are registered, business actions remain explicit',()=>{
 for(const label of ['Next preview page','Review next batch','More matches','More tests','More tests · 20 / 100','Older runs','Load older runs','Older occurrences','Older revisions','More result changes','More changed files','More text','More test choices','More diagnostics','More artifacts','More provider runs','Load more','Load more runs','Load more archives'])assert.equal(isPageLabel(label),true,label);
 for(const label of ['Create archive','Fetch artifact','Import recent runs','Continue import','Continue analysis','New pipeline','Stop import','Retry loading','Load draft','Restore as new revision','Run'])assert.equal(isPageLabel(label),false,label);
});

test('virtual list keeps unchanged rows and scroll position during live updates',async()=>{
 const {mountVirtualList}=await import('../src/virtual-list.mjs');
 const previous=globalThis.ResizeObserver;
 globalThis.ResizeObserver=class{observe(){}disconnect(){}};
 class Node{
  constructor(){this.children=[];this.dataset={};this.style={};this.scrollTop=0;this.clientHeight=64;this.scrollHeight=128;this.isConnected=true;this.ownerDocument={createElement:()=>new Node()};}
  append(...nodes){this.children.push(...nodes);}
  replaceChildren(...nodes){this.children=nodes;}
  setAttribute(){}addEventListener(){}removeEventListener(){}
  getBoundingClientRect(){return{height:32};}
 }
 try{
  const view=new Node(),list=mountVirtualList(view,{key:item=>item.id,version:JSON.stringify,render:item=>Object.assign(new Node(),{text:item.name})});
  list.setItems([{id:'a',name:'A'},{id:'b',name:'B'}]);view.scrollTop=16;
  const body=view.children[1],[a,b]=body.children;
  list.setItems([{id:'a',name:'A'},{id:'b',name:'B2'}]);
  assert.equal(body.children[0],a);assert.notEqual(body.children[1],b);assert.equal(body.children[1].text,'B2');assert.equal(view.scrollTop,16);
  list.destroy();
 }finally{globalThis.ResizeObserver=previous;}
});

test('hidden notification lists keep their estimated heights and open at the beginning',async()=>{
 const {mountVirtualList}=await import('../src/virtual-list.mjs');const previous=globalThis.ResizeObserver;let resize;
 globalThis.ResizeObserver=class{constructor(callback){resize=callback;}observe(){}disconnect(){}};
 class Node{
  constructor(){this.children=[];this.dataset={};this.style={};this.scrollTop=0;this.clientHeight=0;this.scrollHeight=0;this.isConnected=true;this.ownerDocument={createElement:()=>new Node()};}
  append(...nodes){this.children.push(...nodes);}replaceChildren(...nodes){this.children=nodes;}setAttribute(){}addEventListener(){}removeEventListener(){}getBoundingClientRect(){return{height:0};}
 }
 try{const view=new Node(),list=mountVirtualList(view,{key:x=>x,render:()=>new Node(),estimate:80,windowSize:60});list.setItems(Array.from({length:305},(_,i)=>i));resize();assert.equal(view.scrollTop,0);assert.equal(view.children[2].style.height,'19600px');view.clientHeight=400;view.scrollHeight=24400;resize();assert.equal(view.scrollTop,0);assert.equal(view.children[1].children[0].dataset.virtualKey,'0');list.destroy();}finally{globalThis.ResizeObserver=previous;}
});

test('updating a focused queue row restores its action without scrolling',async()=>{
 const {mountVirtualList}=await import('../src/virtual-list.mjs');const previous=globalThis.ResizeObserver;
 globalThis.ResizeObserver=class{observe(){}disconnect(){}};
 const doc={activeElement:null,createElement:()=>new Node()};
 class Node{
  constructor(){this.children=[];this.dataset={};this.style={};this.scrollTop=0;this.clientHeight=64;this.scrollHeight=128;this.isConnected=true;this.ownerDocument=doc;}
  append(...nodes){this.children.push(...nodes);}replaceChildren(...nodes){this.children=nodes;doc.activeElement=null;}setAttribute(){}addEventListener(){}removeEventListener(){}getBoundingClientRect(){return{height:32};}
  contains(node){return this===node||this.children.includes(node);}querySelectorAll(){return this.children;}focus(options){assert.equal(options.preventScroll,true);doc.activeElement=this;}
 }
 try{const view=new Node(),list=mountVirtualList(view,{key:x=>x.id,version:JSON.stringify,render:()=>{const row=new Node();row.append(new Node());return row;}});list.setItems([{id:'run',state:'running'}]);doc.activeElement=view.children[1].children[0].children[0];view.scrollTop=12;list.setItems([{id:'run',state:'succeeded'}]);assert.equal(doc.activeElement,view.children[1].children[0].children[0]);assert.equal(view.scrollTop,12);list.destroy();}finally{globalThis.ResizeObserver=previous;}
});
