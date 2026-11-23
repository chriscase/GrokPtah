import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { inspectMembers, requiredMembers, run } from './verify-public.mjs';
import { desktop } from './stage-public-package.mjs';
function fixture() { const members = new Map(requiredMembers.map(n=>[n,'export {};'])); members.set('package.json',readFileSync(join(desktop,'public-package.json'),'utf8')); return members; }
test('valid synthetic inventory admitted',()=>assert.doesNotThrow(()=>inspectMembers(fixture(),['RESTRICTED_SENTINEL'])));
for (const [name,edit] of [
  ['restricted payload',m=>m.set('public.js','RESTRICTED_SENTINEL')],
  ['native import',m=>m.set('public.js','export {invoke} from "@tauri-apps/api/core";')],
  ['unresolved module',m=>m.set('public.js','export * from "./missing.js";')],
  ['bare dependency',m=>m.set('public.js','export * from "unapproved";')],
  ['computed import',m=>m.set('public.js','export const load = name => import(name);')],
  ['missing runtime export',m=>m.delete('public.js')],
  ['missing declaration',m=>m.delete('types/public.d.ts')],
  ['unexpected member',m=>m.set('private.json','{}')],
  ['manifest drift',m=>m.set('package.json','{}')],
]) test(name+' rejected through verifier',()=>{const m=fixture();edit(m);assert.throws(()=>inspectMembers(m,['RESTRICTED_SENTINEL']));});
test('failed consumer process is a failed gate',async()=>assert.rejects(()=>run(process.execPath,['-e','process.exit(9)'])));
test('hung child is bounded',async()=>assert.rejects(()=>run(process.execPath,['-e','setInterval(()=>{},1000)'],{timeout:100})));

test('TERM-resistant owned child is killed and reaped',async()=>{const start=Date.now();await assert.rejects(()=>run(process.execPath,['-e',"process.on('SIGTERM',()=>{});setInterval(()=>{},1000)"],{timeout:200}));assert.ok(Date.now()-start<3000);});

test('TERM-resistant descendant holding pipes is killed with its group',async()=>{const start=Date.now();const code="const {spawn}=require('node:child_process');spawn(process.execPath,['-e',\"process.on('SIGTERM',()=>{});setInterval(()=>{},1000)\"],{stdio:'inherit'});process.on('SIGTERM',()=>{});setInterval(()=>{},1000)";await assert.rejects(()=>run(process.execPath,['-e',code],{timeout:200}));assert.ok(Date.now()-start<3000);});

const MiB = 1024 * 1024;
for (const channel of ['stdout','stderr']) {
  test(`reject multi-chunk ${channel} above the combined limit`, async()=>{
    const code = `const out=process.${channel}; for(let i=0;i<65;i++) out.write(Buffer.alloc(65536,65));`;
    await assert.rejects(run(process.execPath,['-e',code]), error=>JSON.parse(error.message).failure === 'output limit');
  });
}
test('combine stdout and stderr bytes at the exact boundary',async()=>{
  const runMixed = extra => run(process.execPath,['-e',
    `process.stdout.write(Buffer.alloc(3*1024*1024,65));process.stderr.write(Buffer.alloc(1024*1024+${extra},66));`]);
  assert.equal((await runMixed(0)).length,3*MiB);
  await assert.rejects(runMixed(1),error=>JSON.parse(error.message).failure === 'output limit');
});
test('read a whole in-limit tar member including its multibyte tail',async()=>{
  const root=mkdtempSync(join(tmpdir(),'public-verifier-member-'));
  try {
    const payload='a'.repeat(3*MiB)+'🌱';
    writeFileSync(join(root,'member.txt'),payload);
    await run('tar',['-czf','member.tgz','member.txt'],{cwd:root});
    const read=await run('tar',['-xOzf','member.tgz','member.txt'],{cwd:root});
    assert.equal(read.length,payload.length);
    assert.equal(read,payload);
  } finally { rmSync(root,{recursive:true,force:true}); }
});
test('reject non-text stdout instead of substituting characters',async()=>{
  await assert.rejects(run(process.execPath,['-e','process.stdout.write(Buffer.from([255]))']),
    error=>JSON.parse(error.message).failure === 'invalid UTF-8 stdout');
});
