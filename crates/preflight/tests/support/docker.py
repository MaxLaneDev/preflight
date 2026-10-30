#!/usr/bin/env python3
"""Owned deterministic Docker-protocol fixture; never executes a pipeline command."""
import hashlib,json,pathlib,sys
root=pathlib.Path(__file__).parent
args=sys.argv[1:]
if args[:1]==['--host']:args=args[2:]
state=root/'container.json'
if args[:2]==['context','inspect']:
 print(json.dumps('unix:///preflight-test-only.sock'))
 if (root/'rotate-runtime').exists():
  (root/'rotate-runtime').unlink();script=pathlib.Path(__file__);script.write_text(script.read_text()+'\n# rotated runtime\n')
elif args[:2]==['container','create']:
 labels={}
 for n,arg in enumerate(args[:-1]):
  if arg=='--label':
   key,value=args[n+1].split('=',1);labels[key]=value
 cid=hashlib.sha256(labels['io.preflight.owner'].encode()).hexdigest()
 record={'id':cid,'labels':labels,'image':'sha256:'+'a'*64,'state':{'Running':False,'Status':'created','ExitCode':0}}
 if (root/'uncertain-create').exists():
  (root/'pending-container.json').write_text(json.dumps(record));sys.exit('fixture lost create acknowledgement')
 state.write_text(json.dumps(record));print(cid)
elif args[:2]==['container','ls']:
 if state.exists():print(json.loads(state.read_text())['id'])
elif args[:2]==['container','inspect']:
 record=json.loads(state.read_text())
 if (root/'foreign-label').exists():record['labels']['io.preflight.owner']='foreign'
 print(json.dumps(record))
elif args[:2]==['image','inspect']:
 print(json.dumps({'id':'sha256:'+'a'*64,'os':'linux','arch':'arm64'}))
elif args[:2]==['container','start']:
 record=json.loads(state.read_text());record['state']={'Running':False,'Status':'exited','ExitCode':0,'OOMKilled':False,'StartedAt':'2026-10-30T00:00:00Z','FinishedAt':'2026-10-30T00:00:01Z'};state.write_text(json.dumps(record));print('fixture output')
elif args[:2]==['container','rm']:
 if (root/'refuse-remove').exists():sys.exit('fixture daemon refuses removal')
 state.unlink();print(args[-1])
else:sys.exit('unsupported fixture request')
