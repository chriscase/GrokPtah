import { spawnSync } from 'node:child_process';
import { readFileSync, mkdtempSync, rmSync, writeFileSync, mkdirSync, copyFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { desktop, stage } from './stage-public-package.mjs';
import { runConsumer } from './run-public-consumer-smoke.mjs';
const types = ['public','uiCore','promptQueue','streamApply','streamMarkdown','help/publicSurface','help/generated/contract','help/canonical/corpus','help/canonical/schema','help/canonical/verify','help/canonical/digest','help/retrieval/hybrid','help/retrieval/text','help/verify'];
export const requiredMembers = ['package.json','public.js','ui-core.js',...types.map(x => `types/${x}.d.ts`)];
const deadline = Date.now() + 300000;
export function run(command, args, options = {}) {
  const remaining = deadline - Date.now();
  if (remaining < 1000) throw new Error("qualification deadline exhausted");
  const result = spawnSync(command, args, { timeout: 60000, maxBuffer: 4 * 1024 * 1024, encoding: 'utf8', ...options, timeout: Math.min(options.timeout ?? 60000, remaining) });
  if (result.error || result.status !== 0 || result.signal) throw new Error(`${command} ${args.join(' ')} failed: ${result.error ?? result.signal ?? result.status}\n${result.stdout ?? ''}\n${result.stderr ?? ''}`);
  return result.stdout;
}
export function isolatedEnv(root) {
  mkdirSync(join(root, 'home'), { recursive: true });
  return { PATH: process.env.PATH, HOME: join(root,'home'), TMPDIR: root, npm_config_cache: join(root,'cache'), npm_config_userconfig: join(root,'empty.npmrc'), npm_config_globalconfig: join(root,'empty-global.npmrc'), npm_config_offline: 'true', npm_config_ignore_scripts: 'true', npm_config_audit: 'false', npm_config_fund: 'false' };
}
export function inspectMembers(members, restricted) {
  const names = [...members.keys()];
  if (new Set(names).size !== names.length) throw new Error('duplicate member');
  for (const name of requiredMembers) if (!members.has(name)) throw new Error(`missing member ${name}`);
  for (const name of names) {
    if (!requiredMembers.includes(name) && name !== 'shared.js') throw new Error(`unexpected member ${name}`);
    const bytes = members.get(name);
    if (/\x00/.test(bytes)) throw new Error(`binary member ${name}`);
    for (const probe of restricted) {
      if (probe && (bytes.includes(probe) || bytes.includes(JSON.stringify(probe).slice(1,-1)))) throw new Error(`restricted content in ${name}`);
    }
    if (/@tauri-apps|__TAURI|help-corpus\.v1\.json|createHelpAuthority|helpAsk|helpFollow|helpCancel|invoke\s*\(|(?:from\s*|import\s*\()["'](?:node:|https?:|@)/.test(bytes)) throw new Error(`forbidden native/authority/provider import in ${name}`);
  }
  // Every emitted module edge must resolve inside the inspected artifact. No
  // bare dependencies, network imports, native execution or undeclared files.
  for (const [name, bytes] of members) {
    if (!/\.(?:js|ts)$/.test(name)) continue;
    for (const [, specifier] of bytes.matchAll(/(?:from\s*|import\s*\(|import\s*)["']([^"']+)["']/g)) {
      if (!specifier.startsWith('.')) throw new Error(`external import in ${name}: ${specifier}`);
      const target = new URL(specifier, `https://artifact.invalid/${name}`).pathname.slice(1);
      const declaration = target.replace(/\.js$/, '.d.ts');
      if (!members.has(target) && !(name.endsWith('.d.ts') && members.has(declaration))) throw new Error(`unresolved import ${name}: ${specifier}`);
    }
    if (/\brequire\s*\(|\beval\s*\(|new\s+Function\s*\(/.test(bytes)) throw new Error(`dynamic execution in ${name}`);
  }
  const manifest = JSON.parse(members.get('package.json'));
  if (manifest.name !== '@grokptah/client' || manifest.type !== 'module' || manifest.scripts || manifest.dependencies) throw new Error('unsafe manifest');
  const expected = JSON.parse(readFileSync(join(desktop,'public-package.json'),'utf8'));
  if (JSON.stringify(manifest) !== JSON.stringify(expected)) throw new Error('manifest drift');
  for (const entry of Object.values(manifest.exports)) for (const target of Object.values(entry)) if (!members.has(target.slice(2))) throw new Error(`missing export ${target}`);
}
export function verifyPublic() {
  const root = mkdtempSync(join(tmpdir(), 'grokptah-public-'));
  const env = isolatedEnv(root);
  try {
    run('npm', ['run','build:public'], { cwd: desktop, env, timeout: 120000 });
    const packed = JSON.parse(run('npm',['pack',stage,'--json','--ignore-scripts','--offline','--pack-destination',root],{cwd:root,env}));
    if (packed.length !== 1) throw new Error('expected one packed artifact');
    const tar = join(root, packed[0].filename);
    const listing = run('tar',['-tzf',tar]).trim().split('\n');
    if (new Set(listing).size !== listing.length) throw new Error('duplicate tar member');
    const members = new Map();
    for (const path of listing) {
      if (!path.startsWith('package/') || path.includes('..') || path.endsWith('/')) throw new Error(`unsafe tar member ${path}`);
      members.set(path.slice(8),run('tar',['-xOzf',tar,path]));
    }
    const npmNames = packed[0].files.map(f => f.path).sort();
    if (JSON.stringify(npmNames) !== JSON.stringify([...members.keys()].sort())) throw new Error('pack inventory mismatch');
    const full = JSON.parse(readFileSync(join(desktop,'src/lib/help/canonical/help-corpus.v1.json'),'utf8'));
    const restricted = [...full.chunks.filter(c=>c.visibility !== 'public').map(c=>c.text),...full.sources.filter(s=>s.visibility !== 'public').map(s=>s.id)];
    if (!restricted.length) throw new Error('no restricted probes');
    inspectMembers(members,restricted);
    copyFileSync(tar, join(desktop,'dist/public-client.tgz'));
    runConsumer(tar,root,env);
    const evidence = { sha256:createHash('sha256').update(readFileSync(tar)).digest('hex'), pack:packed[0], members:[...members.keys()].sort(), node:process.version };
    writeFileSync(join(stage,'verification-evidence.json'),JSON.stringify(evidence,null,2)+'\n');
    console.log(JSON.stringify(evidence,null,2));
  } catch (error) {
    mkdirSync(join(desktop,'dist'),{recursive:true});
    writeFileSync(join(desktop,'dist/public-failure.log'),String(error.stack ?? error));
    throw error;
  } finally { rmSync(root,{recursive:true,force:true}); }
  console.log('Public package and offline consumer verified; disposable files removed.');
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) verifyPublic();
