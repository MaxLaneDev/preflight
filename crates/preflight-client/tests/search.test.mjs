import test from 'node:test';
import assert from 'node:assert/strict';
import {groupSearchResults} from '../src/search.mjs';
test('search groups types in a consistent order and preserves rank within each group',()=>{
 const items=[{kind:'log',key:'l'},{kind:'test',key:'t1'},{kind:'project',key:'p'},{kind:'test',key:'t2'},{kind:'module',key:'m'},{kind:'artifact',key:'a'}];
 const original=[...items];const groups=groupSearchResults(items);
 assert.deepEqual(groups.map(g=>g.label),['Projects','Tests','Artifacts','Logs']);
 assert.deepEqual(groups[1].items.map(i=>i.key),['t1','t2']);
 assert.deepEqual(groups[2].items.map(i=>i.key),['m','a']);
 assert.deepEqual(items,original);
});
test('empty and future search kinds do not create empty headings or lose hits',()=>{
 assert.deepEqual(groupSearchResults([]),[]);
 const item={kind:'future-kind',key:'new'};
 assert.deepEqual(groupSearchResults([item]).map(g=>[g.label,g.items]),[['Other',[item]]]);
});
