import test from 'node:test';import assert from 'node:assert/strict';import {preparedImageChoices,choosePreparedImage} from '../src/environment-editor.mjs';
test('prepared image choices use file artifacts and bind producer dependencies',()=>{
 const stages=[{id:'prepare',outputs:{image:{type:'file'},tree:{type:'directory'}}},{id:'test',outputs:{own:{type:'file'}}}];
 const choices=preparedImageChoices(stages,'test');assert.deepEqual(choices,[{value:'${artifact.prepare.image}',label:'prepare / image',producer:'prepare'}]);
 const stage={container:{image:'old',platform:'linux/arm64'},needs:['build']};choosePreparedImage(stage,choices[0].value);assert.equal(stage.container.image_from,choices[0].value);assert(!('image' in stage.container));assert.deepEqual(stage.needs,['build','prepare']);choosePreparedImage(stage,choices[0].value);assert.equal(stage.needs.length,2);choosePreparedImage(stage,'');assert(!stage.container.image_from);assert.equal(stage.container.image,'');assert.throws(()=>choosePreparedImage(stage,'untrusted'));
});
