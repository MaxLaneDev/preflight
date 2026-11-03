import test from 'node:test';
import assert from 'node:assert/strict';
import {notificationJournal} from '../src/notification-center.mjs';
function fixture(){const values=new Map();return {getItem:key=>values.get(key)||null,setItem:(key,value)=>values.set(key,value)};}
test('transient toast removal cannot remove retained notifications; reload preserves unread state',()=>{
 const storage=fixture(),journal=notificationJournal(storage,{now:()=>1000,id:()=> 'one'});
 journal.add('Run completed');journal.add('Failed to save',true);
 const restored=notificationJournal(storage);assert.equal(restored.items.length,2);assert.equal(restored.items[0].kind,'error');assert.equal(restored.items[0].read,false);
 restored.markRead();assert(notificationJournal(storage).items.every(item=>item.read));
});
test('repeat notifications coalesce, copied confirmations stay transient and history is bounded',()=>{
 const journal=notificationJournal(fixture(),{now:()=>1000,id:()=> 'id'});
 journal.add('Copied.');assert.equal(journal.items.length,0);
 journal.add('Connection lost',true);journal.markRead();journal.add('Connection lost',true);assert.equal(journal.items[0].count,2);assert.equal(journal.items[0].read,false);
 for(let i=0;i<205;i++)journal.add('Run '+i);assert.equal(journal.items.length,200);assert.equal(journal.items[0].message,'Run 204');
});
test('bad or unavailable storage does not break notification delivery',()=>{
 const storage={getItem:()=>'{broken',setItem:()=>{throw Error('quota');}};
 const journal=notificationJournal(storage,{id:()=> 'one'});journal.add('<script>untrusted message</script>');assert.equal(journal.items[0].message,'<script>untrusted message</script>');assert.equal(notificationJournal({getItem:()=>'{"length":1}'}).items.length,0);
});
