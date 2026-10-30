import test from 'node:test';
import assert from 'node:assert/strict';
import {LogWindow} from '../src/log-window.mjs';
const events=(from,to)=>Array.from({length:to-from+1},(_,i)=>({id:from+i,text:'line\n'}));
test('tail opens directly and keeps bounded pages in both directions',()=>{
 const w=new LogWindow(6);w.replace(events(15,16),true);assert.equal(w.first,15);assert.equal(w.last,16);assert(w.older);assert(!w.newer);
 w.prepend(events(11,14),true);w.prepend(events(7,10),true);assert.deepEqual(w.events.map(e=>e.id),[7,8,9,10,11,12]);assert(w.newer);
 w.append(events(13,16),false);assert.deepEqual(w.events.map(e=>e.id),[11,12,13,14,15,16]);assert(w.older);assert(!w.newer);
 w.prepend(events(1,10),false);assert.equal(w.events.length,6);assert(!w.older);assert(w.newer);
});
test('empty tails, live append and replay do not duplicate chunks',()=>{
 const w=new LogWindow();w.replace([],false);assert.equal(w.last,0);
 w.append(events(1,3),false);w.append(events(3,5),false);assert.deepEqual(w.events.map(e=>e.id),[1,2,3,4,5]);
 w.replace(events(40,42),true);assert.equal(w.first,40);assert(!w.newer);
});
