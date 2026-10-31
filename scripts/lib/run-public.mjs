import {clientCheck} from './public-acceptance.mjs';
import {withFixture,assert} from './fixture.mjs';
const [scenario,...args]=process.argv.slice(2);
const handlers={'test-client':clientCheck};
if(scenario==='--list'){console.log(Object.keys(handlers).join('\n'));process.exit(0);}
assert(handlers[scenario],'Unknown public fixture: '+scenario);
process.umask(0o077);
await withFixture(scenario,handlers[scenario]);
