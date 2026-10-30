import test from 'node:test';
import assert from 'node:assert/strict';
import {msg,formatted,statusLabel,uiError} from '../src/localization.mjs';
test('interface stays English with a legacy Russian preference and browser locale',async()=>{
 const originalNavigator=Object.getOwnPropertyDescriptor(globalThis,'navigator'),originalStorage=Object.getOwnPropertyDescriptor(globalThis,'localStorage');
 try{
  Object.defineProperty(globalThis,'navigator',{value:{language:'ru-RU'},configurable:true});
  Object.defineProperty(globalThis,'localStorage',{value:{getItem:()=> 'ru'},configurable:true});
  const client=await import('../src/localization.mjs?legacy-preference');
  assert.equal(String(client.msg('Settings')),'Settings');assert.equal(client.statusLabel('running'),'running');
  assert.equal(String(client.formatted(12345.5)),(12345.5).toLocaleString('en'));
  assert.equal('setLanguage' in client,false);assert.equal('editorPhrases' in client,false);
 }finally{
  for(const [key,descriptor]of [['navigator',originalNavigator],['localStorage',originalStorage]]){if(descriptor)Object.defineProperty(globalThis,key,descriptor);else delete globalThis[key];}
 }
});
test('message parameters and API data remain literal',()=>{
 const raw={name:'Settings',program:'Close',path:'Logs/Tests',text:'<script>{0}</script>',status:'running'},bytes=JSON.stringify(raw);
 assert.equal(String(msg('Run {0} · {1}',{0:raw.name,1:statusLabel(raw.status)})),'Run Settings · running');
 assert.equal(String(msg('{0}',{0:raw.text})),raw.text);assert.equal(JSON.stringify(raw),bytes);
 assert.throws(()=>JSON.stringify({name:msg('Settings')}),/cannot be serialized/);assert.equal(String(uiError(msg('Settings')).localizedMessage),'Settings');
});
test('dates and numbers use English formatting',()=>{
 assert.equal(String(formatted(1234567.5)),(1234567.5).toLocaleString('en'));
 const date=new Date('2026-10-29T13:04:05Z');const text=String(formatted(date,'toLocaleString',{timeZone:'UTC'}));assert.match(text,/29\/10\/2026/);assert.match(text,/13:04:05/);assert.doesNotMatch(text,/AM|PM/);
 assert.match(String(formatted(new Date('2026-10-29T00:00:00Z'),'toLocaleTimeString',{timeZone:'UTC'})),/^00:00:00$/);
});
