import {test} from 'node:test';
import {fileURLToPath} from 'node:url';
import {chromium} from '../../firecrab-e2e/node_modules/playwright-core/index.mjs';
import {expect} from '../../firecrab-e2e/node_modules/@playwright/test/index.mjs';
import {createFixtureServer} from './fixture-server.mjs';
test('pool loading never exposes a New pool button that disappears when the empty form arrives',async()=>{
 const fixture=await createFixtureServer(fileURLToPath(new URL('../dist',import.meta.url)),0);
 const browser=await chromium.launch({headless:true});let release;
 try {
  const page=await browser.newPage();let observed;
  const request=new Promise(resolve=>observed=resolve),gate=new Promise(resolve=>release=resolve);
  await page.route('**/api/pools',async route=>{observed();await gate;await route.fulfill({status:200,contentType:'application/json',body:'[]'});});
  await page.goto(fixture.url+'/#/pools');await request;
  await expect(page.getByRole('button',{name:'New pool',exact:true})).toHaveCount(0);
  release();await expect(page.getByRole('region',{name:'Create a pool',exact:true})).toBeVisible();
 }finally{release?.();await browser.close();await fixture.close();}
});
