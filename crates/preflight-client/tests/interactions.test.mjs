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
