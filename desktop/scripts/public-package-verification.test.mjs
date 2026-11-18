import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { inspectMembers, requiredMembers, run } from './verify-public.mjs';
import { desktop } from './stage-public-package.mjs';
function fixture() { const members = new Map(requiredMembers.map(n=>[n,'export {};'])); members.set('package.json',readFileSync(join(desktop,'public-package.json'),'utf8')); return members; }
test('valid synthetic inventory admitted',()=>assert.doesNotThrow(()=>inspectMembers(fixture(),['RESTRICTED_SENTINEL'])));
for (const [name,edit] of [
  ['restricted payload',m=>m.set('public.js','RESTRICTED_SENTINEL')],
  ['native import',m=>m.set('public.js','export {invoke} from "@tauri-apps/api/core";')],
  ['missing runtime export',m=>m.delete('public.js')],
  ['missing declaration',m=>m.delete('types/public.d.ts')],
  ['unexpected member',m=>m.set('private.json','{}')],
  ['manifest drift',m=>m.set('package.json','{}')],
]) test(name+' rejected through verifier',()=>{const m=fixture();edit(m);assert.throws(()=>inspectMembers(m,['RESTRICTED_SENTINEL']));});
test('failed consumer process is a failed gate',async()=>assert.rejects(()=>run(process.execPath,['-e','process.exit(9)'])));
test('hung child is bounded',async()=>assert.rejects(()=>run(process.execPath,['-e','setInterval(()=>{},1000)'],{timeout:100})));

test('TERM-resistant owned child is killed and reaped',async()=>{const start=Date.now();await assert.rejects(()=>run(process.execPath,['-e',"process.on('SIGTERM',()=>{});setInterval(()=>{},1000)"],{timeout:200}));assert.ok(Date.now()-start<3000);});

test('TERM-resistant descendant holding pipes is killed with its group',async()=>{const start=Date.now();const code="const {spawn}=require('node:child_process');spawn(process.execPath,['-e',\"process.on('SIGTERM',()=>{});setInterval(()=>{},1000)\"],{stdio:'inherit'});process.on('SIGTERM',()=>{});setInterval(()=>{},1000)";await assert.rejects(()=>run(process.execPath,['-e',code],{timeout:200}));assert.ok(Date.now()-start<3000);});
