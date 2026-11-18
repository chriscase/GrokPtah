# Offline public client package

`desktop` now defines a locally packable `@grokptah/client`. It is not published to a registry. This milestone does not qualify native transports, providers, durability, or deployment.

- Root: the unchanged `help/publicSurface.ts` contract, public corpus/digest, offline lexical/BM25/trigram retrieval and stricter citation verification.
- `@grokptah/client/ui-core`: the same public Help plus current pure prompt queue, stream application and streaming Markdown helpers.
- No private corpus, authority constructor, Tauri bridge, broker transport, provider executor, or styled UI. #420 remains separate.

From `desktop`, using its exact `package-lock.json` install:

```
npm run test:public-verifier
npm run verify:public
npm run test -- src/lib/public.test.ts src/lib/uiCore.test.ts
npm run typecheck
npm run build
npm run verify:help-public
```

`verify:public` builds bundled ESM and declarations, stages the explicit manifest, npm-packs it, inspects every member and verifies the export/declaration closure. It rejects unexpected members, restricted corpus content and native/external module edges. A fresh temporary project outside the checkout installs that tarball offline, without install scripts or personal npm configuration, compiles a NodeNext TypeScript consumer and runs real package-name imports. It exercises public corpus verification, retrieval/abstention, forged and missing citations, and pure helpers.

The owned consumer is cleaned in a finally block. Cleanup failure is failure. Owned process groups receive TERM followed by KILL on timeout or excess output; TERM-resistant children and grandchildren are regression controls. Child output is bounded to 4 MiB, each command is time-limited and the qualification has a five-minute deadline. Failures retain diagnostic output in `dist/public-failure.log`. Successful evidence (source commit/tree/parent/status, lock SHA256, tool/npm/Node identities, command exits/streams, cleanup result, manifest, exact pack member inventory and tar SHA256) is in `dist/public/verification-evidence.json`; the original tarball is retained as `dist/public-client.tgz`. These are local artifacts, not package exports. Synthetic controls never edit real corpus data.

## Source provenance and qualification limits

Receiving base: `7ddce0d5af8b7d4ac8a3e27e839ab6a232ab141e`. Selective design donor: #428 `d57ad0be37443e51414a6ed92d0b8e3a6217b1da` (explicit export map and separate build configuration). No donor broker or authority code was copied. Existing Help projection, corpus/codegen, application configuration, dependencies and lock remain unchanged; their historical evidence is not new-package CI.

Implementation-time checks: eleven dependency-free verifier controls and declaration emission using TypeScript 5.9.3 pass. Full build, packed consumer and app checks require the exact receiving-lock closure. The existing main checkout install was Vite 5.4.21/Vitest 2.1.9/esbuild 0.21.5/Rollup 4.62.2, while this source lock requires 7.3.6/3.2.7/0.28.1/4.62.3. That install was not reused or mutated. Until matching input is admitted and candidate-bound checks pass, this is **not qualified for merge or publication**.
