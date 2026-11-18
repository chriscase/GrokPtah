import { describe, expect, it } from 'vitest';
import * as client from './public';
describe('public client',()=>{
  it('exports only the current public Help surface', async()=>{
    expect(Object.keys(client).sort()).toEqual(Object.keys(await import('./help/publicSurface')).sort());
    expect(client.searchHelpCorpus('').kind).toBe('abstained');
    expect(()=>client.assertPublicOnly(client.HELP_PUBLIC_CORPUS)).not.toThrow();
  });
});
