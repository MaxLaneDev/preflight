import test from 'node:test';
import assert from 'node:assert/strict';
import {ConsoleLines,describeLogLine} from '../src/console.mjs';
const event=(text,stream='stdout',stage='test')=>({text,stream,stage_id:stage,timestamp:1});
test('wrapper records expose actual diagnostics/test lines and preserve metadata',()=>{
 const value={label:'private-owner',exitCode:1,elapsedMs:13,stdout:'/tmp/stdout.log',stderr:'/tmp/stderr.log',diagnostics:["error: symbol is package-private"],testLines:[]};
 const data=describeLogLine({text:JSON.stringify(value),complete:true});
 assert.equal(data.title,'private-owner');assert.equal(data.meta,'exit 1 · 13 ms');
 assert.deepEqual(data.messages,value.diagnostics);assert.equal(data.tone,null);
 assert.deepEqual(data.value,value);
});
test('reported check outcome remains distinct from subprocess exit code',()=>{
 const data=describeLogLine({text:'{"name":"expected rejection","passed":true,"exitCode":1,"testLines":["test control ... ok"]}',complete:true});
 assert.equal(data.tone,'passed');assert.equal(data.meta,'check passed · exit 1');
 assert.deepEqual(data.messages,['test control ... ok']);
});
test('JSON split across chunks and pages is assembled before formatting',()=>{
 const model=new ConsoleLines();
 model.append([event('{"label":"con')]);assert.equal(describeLogLine(model.lines[0]),null);
 model.append([event('trol","testLines":["test a ... ok"]}\nnext\n')]);
 assert.equal(model.lines.length,2);assert.equal(describeLogLine(model.lines[0]).title,'control');
 assert.equal(model.lines[1].text,'next\n');
});
test('streams and stages never concatenate each other into a JSON record',()=>{
 const model=new ConsoleLines();model.append([event('{"label":'),event('other\n','stderr'),event('"a"}\n'),event('second\n','stdout','another')]);
 assert.equal(describeLogLine(model.lines[0]).title,'a');assert.equal(model.lines[1].stream,'stderr');
 assert.equal(model.lines[2].stage,'another');assert.equal(model.lines[2].newGroup,true);
});
test('terminal partial lines render without inventing a newline',()=>{
 const model=new ConsoleLines();model.append([event('{"message":"complete"}')]);model.finish();
 assert.equal(describeLogLine(model.lines[0]).title,'complete');
 assert.equal(model.rawText(),'[test/stdout] {"message":"complete"}');
});
test('plain text, invalid JSON and untrusted HTML stay literal data',()=>{
 for(const text of ['compiler error\n','{broken}\n','1\n','null\n','"<script>"\n'])assert.equal(describeLogLine({text,complete:true}),null);
 const data=describeLogLine({text:'{"message":"<img src=x onerror=alert(1)>"}',complete:true});
 assert.equal(data.title,'<img src=x onerror=alert(1)>');
});
test('bounded console evicts old rows and safely handles huge unfinished lines',()=>{
 const model=new ConsoleLines({maxCharacters:20,maxLines:2});
 model.append([event('a\nb\nc\n')]);assert.equal(model.lines.length,2);assert(model.dropped);
 model.append([event('x'.repeat(100))]);assert.equal(model.characters,20);assert.equal(model.lines.length,1);
 assert.equal(model.lines[0].text.length,20);assert.equal(model.pending.size,1);
 model.append([event('\n')]);assert(model.lines[0].truncated);assert.equal(describeLogLine(model.lines[0]),null);
});
test('large metadata is bounded and reported output is not counted as tests',()=>{
 const data=describeLogLine({text:JSON.stringify({label:'many',testLines:Array.from({length:220},()=> 'reported line')}),complete:true});
 assert.equal(data.messages.length,201);assert.equal(data.value.testLines.length,220);
 assert.equal(Object.hasOwn(data,'tests'),false);
});

test('evicted partial records never become invented JSON records on continuation',()=>{
 const model=new ConsoleLines({maxCharacters:80,maxLines:2});
 model.append([event('{"original":'),event('a\nb\nc\n','stderr')]);
 model.append([event('{"passed":true}\n')]);
 const continuation=model.lines.at(-1);assert(continuation.truncated);assert.equal(describeLogLine(continuation),null);
 model.append([event('{"message":"next real record"}\n')]);
 assert.equal(describeLogLine(model.lines.at(-1)).title,'next real record');
});
