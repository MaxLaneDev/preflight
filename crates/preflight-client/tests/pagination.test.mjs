import test from 'node:test';
import assert from 'node:assert/strict';
import {isPageLabel} from '../src/virtual-list.mjs';
test('cursor controls are registered, business actions remain explicit',()=>{
 for(const label of ['Next preview page','Review next batch','More matches','More tests','More tests · 20 / 100','Older runs','Load older runs','Older occurrences','Older revisions','More result changes','More changed files','More text','More test choices','More diagnostics','More artifacts','More provider runs','Load more','Load more runs','Load more archives'])assert.equal(isPageLabel(label),true,label);
 for(const label of ['Create archive','Fetch artifact','Import recent runs','Continue import','Continue analysis','New pipeline','Stop import','Retry loading','Load draft','Restore as new revision','Run'])assert.equal(isPageLabel(label),false,label);
});
