import test from 'node:test';
import assert from 'node:assert/strict';
import {closeDialog,animatePage} from '../src/interactions.mjs';
function dialog(finished){return {open:true,closes:0,animations:0,close(value){this.closes++;this.returnValue=value;this.open=false;},animate(){this.animations++;return {finished,cancel(){}};}};}
test('close waits for exit animation and repeated clicks close only once',async()=>{
 globalThis.matchMedia=()=>({matches:false});let finish;const node=dialog(new Promise(resolve=>finish=resolve));
 const first=closeDialog(node,'saved'),second=closeDialog(node,'saved');assert.equal(first,second);assert.equal(node.open,true);assert.equal(node.animations,1);
 finish();await first;assert.equal(node.open,false);assert.equal(node.closes,1);assert.equal(node.returnValue,'saved');await closeDialog(node);assert.equal(node.closes,1);
});
test('animation failure still closes; reduced motion closes immediately',async()=>{
 globalThis.matchMedia=()=>({matches:false});const failed=dialog(Promise.reject(new Error('cancelled')));await closeDialog(failed);assert.equal(failed.closes,1);
 globalThis.matchMedia=()=>({matches:true});const reduced=dialog(Promise.resolve());const done=closeDialog(reduced);assert.equal(reduced.open,false);assert.equal(reduced.animations,0);await done;
});
test('page refresh with the same route does not reanimate',()=>{
 globalThis.matchMedia=()=>({matches:false});const page=dialog(Promise.resolve());animatePage(page,'/pipelines');animatePage(page,'/pipelines');assert.equal(page.animations,1);animatePage(page,'/experiments');assert.equal(page.animations,2);
 globalThis.matchMedia=()=>({matches:true});animatePage(page,'/schedules');assert.equal(page.animations,2);
});

test('dragging a stage moves its edges and returns to layout on release or cancellation',async()=>{
 const {elasticDrag}=await import('../src/interactions.mjs'),handlers=new Map();let redraws=0;
 const node={style:{},addEventListener:(name,fn)=>handlers.set(name,fn),setPointerCapture(){}};
 elasticDrag(node,()=>.5,()=>redraws++);
 const event={button:0,pointerId:7,clientX:10,clientY:20,preventDefault(){},stopImmediatePropagation(){this.stopped=true;}};
 handlers.get('pointerdown')(event);handlers.get('pointermove')({...event,clientX:30,clientY:35});
 assert.equal(node.style.transform,'translate(40px,30px)');assert.equal(redraws,1);
 handlers.get('pointerup')(event);assert.equal(node.style.transform,'');assert.equal(redraws,2);
 handlers.get('click')(event);assert.equal(event.stopped,true);
 handlers.get('pointerdown')(event);handlers.get('pointermove')({...event,clientX:40});handlers.get('pointercancel')(event);assert.equal(node.style.transform,'');
});

async function loadGraphLayout(){
 const {readFile}=await import('node:fs/promises');
 const source=await readFile(new URL('../src/app.js',import.meta.url),'utf8');
 const layout=source.slice(source.indexOf('export function pipelineLevels'),source.indexOf('function graph('));
 return import('data:text/javascript,'+encodeURIComponent(layout));
}

test('pipeline branches stay aligned across dependency levels, with shared children placed once',async()=>{
 const {pipelineLevels,pipelineRows,graphConnector}=await loadGraphLayout();
 const stages=[
  {id:'join',needs:['root','right-end','left-end']},
  {id:'right-end',needs:['right']},
  {id:'root'},
  {id:'left',needs:['root']},
  {id:'right',needs:['root']},
  {id:'left-end',needs:['left']},
 ];
 const levels=pipelineLevels(stages),rows=pipelineRows(stages,levels);
 assert.deepEqual(stages.map(({id})=>levels.get(id)),[3,2,0,1,1,2]);
 assert.equal(rows.size,stages.length);
 assert.equal(rows.get('left'),rows.get('left-end'));
 assert.equal(rows.get('right'),rows.get('right-end'));
 assert.equal(rows.get('root'),(rows.get('left')+rows.get('right'))/2);
 assert.notEqual(rows.get('left'),rows.get('right'));
 assert.equal(graphConnector(0,50,280,80),'M0 50 C140 50 140 80 280 80');
});

for(const [name,dependencies] of [
 ['multiple roots, an isolated stage and two merges',{
  finish:['package','audit'],audit:['lint','integration'],package:['compile','integration'],
  integration:['compile','database'],compile:['source'],lint:['source'],database:[],source:[],isolated:[],
 }],
 ['wide fan-out with cross-branch joins',{
  root:[],a:['root'],b:['root'],c:['root'],d:['root'],e:['root'],
  x:['a','d'],y:['b','e'],z:['c','x','y'],publish:['root','z'],
 }],
 ['Strox-style shortcut with branches above and below',{
  root:[],build:['root'],lookup:['root'],lookupTest:['lookup'],
  cache:['build'],cacheA:['cache'],cacheB:['cache'],
  o0:['build'],o3:['o0','build'],pending:['build'],pendingTest:['pending'],
 }],
]){
 test(`graph grid: ${name}`,async()=>{
  const {pipelineLevels,pipelineRows,pipelineRowOffsets,graphConnector}=await loadGraphLayout();
  const stages=Object.entries(dependencies).map(([id,needs])=>({id,needs}));
  const levels=pipelineLevels(stages),rows=pipelineRows(stages,levels);
  const offsets=pipelineRowOffsets(stages,levels,rows);
  assert.equal(rows.size,stages.length);
  assert.deepEqual(pipelineRows(stages,levels),rows);
  for(const {id,needs} of stages){
   for(const parent of needs){
    assert.ok(levels.get(parent)<levels.get(id),`${parent} must be left of ${id}`);
    const path=graphConnector(levels.get(parent)*232+180,rows.get(parent),levels.get(id)*232,rows.get(id));
    assert.match(path,/^M[\d.]+ [\d.]+ C[\d. ]+$/);
   }
   for(const other of stages){
    if(other.id!==id&&levels.get(other.id)===levels.get(id)){
     assert.ok(Math.abs(rows.get(id)-rows.get(other.id))>=2,`${id} and ${other.id} must have separate slots`);
    }
   }
  }
  if(Object.hasOwn(dependencies,'o3')){
   assert.ok(offsets.get('o3')<0);
   assert.ok(offsets.get('o0')>0);
   assert.equal(offsets.has('lookup'),false);
  }
 });
}

test('graph clears a covered connection by shifting a card or making room in its column',async()=>{
 const {clearGraphEdges}=await loadGraphLayout();
 const rects=new Map([
  ['parent',{left:0,right:180,top:80,bottom:120}],
  ['child',{left:464,right:644,top:80,bottom:120}],
  ['card',{left:232,right:412,top:93,bottom:110}],
 ]);
 const edges=[['parent','child']];
 const cleared=new Map(rects); clearGraphEdges(edges,cleared); assert.ok(cleared.get('card').bottom<(cleared.get('parent').top+cleared.get('parent').bottom)/2-1||cleared.get('card').top>(cleared.get('parent').top+cleared.get('parent').bottom)/2+1);
 rects.set('above',{left:232,right:412,top:48,bottom:85});
 rects.set('below',{left:232,right:412,top:118,bottom:155});
 assert.ok(clearGraphEdges(edges,rects).size>0);
 const ordered=['above','card','below'].map(id=>rects.get(id)).sort((a,b)=>a.top-b.top);
 for(let i=1;i<ordered.length;i++)assert.ok(ordered[i].top-ordered[i-1].bottom>=8);
 const line=(rects.get('parent').top+rects.get('parent').bottom)/2;assert.ok(rects.get('card').bottom<line-1||rects.get('card').top>line+1);
 rects.set('card',{left:232,right:412,top:28,bottom:1000});
 for(const id of ['parent','child'])rects.set(id,{...rects.get(id),top:480,bottom:520}); const crowded=new Map([...rects].filter(([id])=>id!=='above'&&id!=='below'));clearGraphEdges(edges,crowded);assert.ok([...crowded.values()].every(r=>Number.isFinite(r.top)&&r.top>=28));
});

test('graph uncrosses links and shortens clear links',async()=>{
 const {clearGraphEdges}=await loadGraphLayout();
 const rect=(left,top)=>({left,right:left+180,top,bottom:top+40});
 const rects=new Map([['a',rect(0,80)],['b',rect(0,160)],['c',rect(232,80)],['d',rect(232,160)]]);
 clearGraphEdges([['a','d'],['b','c']],rects);
 assert.equal(rects.get('a').top<rects.get('b').top,rects.get('d').top<rects.get('c').top);
 const pair=new Map([['a',rect(0,80)],['b',rect(232,160)]]);
 clearGraphEdges([['a','b']],pair);
 assert.equal(pair.get('a').top,pair.get('b').top);
});

test('crossed links can swap two neighboring cards in place',async()=>{
 const {clearGraphEdges}=await loadGraphLayout();
 const rect=(left,top)=>({left,right:left+180,top,bottom:top+80});
 const rects=new Map([['a',rect(0,28)],['b',rect(0,116)],['b1',rect(232,28)],['a1',rect(232,116)]]);
 clearGraphEdges([['a','a1'],['b','b1']],rects);
 assert.equal(rects.get('a').top,rects.get('a1').top);
 assert.equal(rects.get('b').top,rects.get('b1').top);
 assert.deepEqual([...rects.values()].map(r=>r.top).sort((a,b)=>a-b),[28,28,116,116]);
});

test('audit/package reorder with shared entry and exit ports',async()=>{
 const {clearGraphEdges}=await loadGraphLayout();
 const rect=(left,top)=>({left,right:left+180,top,bottom:top+132});
 const rects=new Map([
  ['compile',rect(0,300)],['lint',rect(0,450)],
  ['integration',rect(232,100)],
  ['audit',rect(464,300)],['package',rect(464,450)],
  ['finish',rect(696,375)],
 ]);
 const edges=[['compile','package'],['lint','audit'],['integration','audit'],['integration','package'],['audit','finish'],['package','finish']];
 clearGraphEdges(edges,rects);
 assert.equal(rects.get('package').top<rects.get('audit').top,rects.get('compile').top<rects.get('lint').top,'branches must keep matching order');
 for(const [from,to] of edges)assert.ok(rects.get(from).right<rects.get(to).left);
});

test('start column packs disconnected roots with a small gap',async()=>{
 const {clearGraphEdges}=await loadGraphLayout();
 const rect=top=>({left:0,right:180,top,bottom:top+120});
 const rects=new Map([['database',rect(28)],['source',rect(400)],['isolated',rect(800)]]);
 clearGraphEdges([],rects);
 assert.equal(rects.get('source').top-rects.get('database').bottom,8);
 assert.equal(rects.get('isolated').top-rects.get('source').bottom,8);
});

test('parent balances outer fan-out branches after length optimization',async()=>{
 const {clearGraphEdges}=await loadGraphLayout();
 const rect=(left,top)=>({left,right:left+180,top,bottom:top+80});
 const rects=new Map([['root',rect(0,500)],['top',rect(232,28)],['middle',rect(232,450)],['bottom',rect(232,550)]]);
 clearGraphEdges([['root','top'],['root','bottom']],rects);
 const center=id=>(rects.get(id).top+rects.get(id).bottom)/2;
 assert.ok(Math.abs(center('root')-(center('top')+center('bottom'))/2)<1);
});

test('balancing allows intermediate child links to grow',async()=>{
 const {clearGraphEdges}=await loadGraphLayout();
 const rect=(left,top)=>({left,right:left+180,top,bottom:top+80});
 const rects=new Map([['root',rect(0,500)],['top',rect(232,28)],['middle',rect(232,500)],['bottom',rect(232,600)]]);
 clearGraphEdges([['root','top'],['root','middle'],['root','bottom']],rects);
 const center=id=>(rects.get(id).top+rects.get(id).bottom)/2;
 assert.ok(Math.abs(center('root')-(center('top')+center('bottom'))/2)<1);
});

for(const count of [4,5])for(const constrained of [false,true]){
 test(`${count} children balance ${constrained?'with':'without'} a neighboring card constraint`,async()=>{
  const {clearGraphEdges}=await loadGraphLayout();
  const rect=(left,top)=>({left,right:left+180,top,bottom:top+80});
  const rects=new Map([['root',rect(0,500)]]);
  const edges=[];
  for(let i=0;i<count;i++){const id=`child-${i}`;rects.set(id,rect(232,28+i*140));edges.push(['root',id]);}
  if(constrained)rects.set('neighbor',rect(0,340));
  clearGraphEdges(edges,rects);
  const roots=rects.get('root'),kids=Array.from({length:count},(_,i)=>rects.get(`child-${i}`)).sort((a,b)=>a.top-b.top);
  if(!constrained){assert.ok(Math.abs((roots.top+roots.bottom)/2-(kids[0].top+kids[0].bottom+kids.at(-1).top+kids.at(-1).bottom)/4)<1);for(let i=1;i<kids.length;i++)assert.ok(kids[i].top-kids[i-1].bottom<=8.01);}
  for(let i=1;i<kids.length;i++)assert.ok(kids[i].top-kids[i-1].bottom>=8);
  if(constrained){const neighbor=rects.get('neighbor');assert.ok(roots.bottom+8<=neighbor.top||neighbor.bottom+8<=roots.top);}
 });
}

test('parallel chains align centers and final drawing removes empty top rows',async()=>{
 const {clearGraphEdges}=await loadGraphLayout();
 const rect=(left,top,height)=>({left,right:left+180,top,bottom:top+height});
 const rects=new Map([['a',rect(0,200,132)],['b',rect(232,196,134)],['c',rect(464,196,134)],['d',rect(0,350,132)],['e',rect(232,348,134)]]);
 clearGraphEdges([['a','b'],['b','c'],['d','e']],rects);
 const center=id=>(rects.get(id).top+rects.get(id).bottom)/2;
 assert.equal(center('a'),center('b'));assert.equal(center('b'),center('c'));assert.equal(center('d'),center('e'));
 assert.equal(Math.min(...[...rects.values()].map(r=>r.top)),28);
});

 test('final compaction lifts a child column with its parent and keeps short uniform gaps',async()=>{
  const {clearGraphEdges}=await loadGraphLayout();
  const rect=(left,top)=>({left,right:left+178,top,bottom:top+132});
  const rects=new Map([['upstream',rect(0,56)],['parent',rect(232,530)],['result',rect(696,28)]]);
  const edges=[['upstream','parent'],['upstream','result'],['child-5','result']];
  for(let i=0;i<5;i++){const id=`child-${[5,2,3,4,1][i]}`;rects.set(id,rect(464,245+i*148));edges.push(['parent',id]);}
  clearGraphEdges(edges,rects);
  const kids=[...rects].filter(([id])=>id.startsWith('child-')).map(([,r])=>r).sort((a,b)=>a.top-b.top);
  assert.ok(kids[0].top<245);assert.ok(rects.get('parent').top<530);
  for(let i=1;i<kids.length;i++)assert.ok(Math.abs(kids[i].top-kids[i-1].bottom-8)<.01);
 });
