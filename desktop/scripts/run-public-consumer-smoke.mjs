import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { desktop } from './stage-public-package.mjs';
import { run } from './verify-public.mjs';
export function runConsumer(tar, root, env) {
  const cwd = join(root,'consumer');
  mkdirSync(cwd);
  writeFileSync(join(cwd,'package.json'),JSON.stringify({private:true,type:'module'}));
  run('npm',['install',tar,'--offline','--ignore-scripts','--no-audit','--no-fund','--package-lock=false'],{cwd,env});
  writeFileSync(join(cwd,'consumer.ts'), `
import * as client from '@grokptah/client';
import * as ui from '@grokptah/client/ui-core';
function check(value: unknown, message: string): asserts value { if (!value) throw new Error(message); }
client.assertPublicOnly(client.HELP_PUBLIC_CORPUS);
client.verifyHelpCorpus(client.HELP_PUBLIC_CORPUS);
check(client.HELP_PUBLIC_CORPUS_DIGEST === client.HELP_PUBLIC_CORPUS.digest, 'digest mismatch');
check(client.searchHelpCorpus('').kind === 'abstained', 'empty query must abstain');
const chunk = client.HELP_PUBLIC_CORPUS.chunks[0];
check(client.searchHelpCorpus(chunk.text).kind === 'results', 'retrieval must answer known corpus text');
const source = client.HELP_PUBLIC_CORPUS.sources[0];
const forged: client.HelpProjection = {handle:'consumer',status:'answered',error:null,message:null,claims:[{ordinal:0,text:'A claim',citations:[{source_id:source.id,path:source.path,heading:source.heading,quote:'NONEXISTENT_PUBLIC_CONSUMER_QUOTE'}]}]};
check(client.verifyHelpProjection(forged).rejected.length === 1, 'forged quote accepted');
const unknown: client.HelpProjection = {...forged,claims:[{...forged.claims[0],citations:[{...forged.claims[0].citations[0],source_id:'unknown-source'}]}]};
check(client.verifyHelpProjection(unknown).rejected.length === 1, 'unknown citation accepted');
check(ui.queueKind('/help') === 'command', 'queue helper');
check(ui.nextAssistantText('line','line') === 'lineline', 'stream repetition');
check(ui.unclosedFenceStart('hello') === -1, 'markdown helper');
for (const api of [client,ui]) for (const name of ['helpAsk','helpFollow','helpCancel','createHelpAuthority','invoke']) check(!(name in api), 'unsafe export '+name);
console.log('Packed consumer imports, declarations, retrieval, citation rejection and UI helpers passed.');
`);
  run(process.execPath,[join(desktop,'node_modules/typescript/bin/tsc'),'consumer.ts','--module','NodeNext','--moduleResolution','NodeNext','--target','ES2022','--strict','--outDir','out'],{cwd,env});
  run(process.execPath,['out/consumer.js'],{cwd,env});
}
