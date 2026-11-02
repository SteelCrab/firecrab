import {test,before,after,beforeEach,afterEach} from 'node:test';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {chromium} from '../../firecrab-e2e/node_modules/playwright-core/index.mjs';
import {expect} from '../../firecrab-e2e/node_modules/@playwright/test/index.mjs';
import {createFixtureServer} from './fixture-server.mjs';
import {installCanvasSkillsFixture} from './canvas-skills-fixture.mjs';
let fixture,browser,context,page,canvas,evidence=0;
before(async()=>{fixture=await createFixtureServer(fileURLToPath(new URL('../dist',import.meta.url)),0);browser=await chromium.launch({headless:true});});
after(async()=>{await browser?.close();await fixture?.close();});
beforeEach(async()=>{fixture.reset(0);canvas=installCanvasSkillsFixture(fixture);context=await browser.newContext({viewport:{width:1440,height:1100}});page=await context.newPage();});
afterEach(async()=>{try{await page?.screenshot({path:fileURLToPath(new URL('../../test-results/canvas-skills/state-'+(++evidence)+'.jpg',import.meta.url)),fullPage:true});}finally{await context?.close();}});
async function open(){
 await page.goto(fixture.url+'/#/ai-canvas');await page.getByLabel('Project',{exact:true}).selectOption(canvas.project);await page.getByRole('button',{name:'Open project',exact:true}).click();await page.getByRole('button',{name:'Folder agent',exact:true}).click();
 await expect(page.getByLabel('Agent model',{exact:true})).toContainText('qa-chatgpt');await page.getByLabel('Agent model',{exact:true}).selectOption({index:1});
 await page.getByRole('button',{name:/Official skills/}).click();await page.getByRole('button',{name:'Refresh openai',exact:true}).click();await expect(page.getByText('Fixture review',{exact:true})).toBeVisible();
}
async function install(){await page.locator('.cw-skill-item').filter({has:page.getByText('Fixture review',{exact:true})}).getByRole('button',{name:'Install in project',exact:true}).click();await expect(page.getByRole('checkbox',{name:'Select skill Fixture review',exact:true})).toBeVisible();}
test('official catalog filters distinguish bundles; installed selection survives reload and becomes inactive on model change',async()=>{
 await open();await expect(page.getByText(/Plugin skill bundle/)).toBeVisible();await page.getByLabel('Search official skills',{exact:true}).fill('review');await expect(page.getByText('Fixture bundle',{exact:true})).toHaveCount(0);
 await install();await page.getByRole('checkbox',{name:'Select skill Fixture review',exact:true}).check();
 await page.getByLabel('Agent request',{exact:true}).fill('Use the selected fixture skill');await page.getByRole('button',{name:'Start file task',exact:true}).click();await expect(page.getByText('Fixture result. No model or VM executed.',{exact:true})).toBeVisible();
 assert.deepEqual(canvas.admissions[0].body.selectedSkillIds,[...canvas.installations.keys()]);
 await page.getByRole('checkbox',{name:'Select skill Fixture review',exact:true}).scrollIntoViewIfNeeded();
 await page.screenshot({path:fileURLToPath(new URL('../../test-results/canvas-skills/installed-selected.jpg',import.meta.url)),fullPage:true});
 await page.getByRole('button',{name:'Save layout',exact:true}).click();await expect(page.locator('#cw-save-state')).toContainText('Saved privately');await page.reload();await expect(page.locator('.cw-skill-selections')).toContainText('Fixture review');
 await page.getByLabel('Agent model',{exact:true}).selectOption({index:2});await expect(page.locator('.cw-skill-selections')).toContainText('Inactive');
 await page.getByLabel('Agent request',{exact:true}).fill('Claude without the OpenAI skill');await page.getByRole('button',{name:'Start file task',exact:true}).click();await expect.poll(()=>canvas.admissions.length).toBe(2);assert.equal(canvas.admissions[1].body.selectedSkillIds,undefined);assert.equal(canvas.installations.size,1);
});
test('lost installation acknowledgements reuse the saved operation; removal retains accepted task data',async()=>{
 await open();canvas.loseNextInstallation();await page.locator('.cw-skill-item').filter({has:page.getByText('Fixture review',{exact:true})}).getByRole('button',{name:'Install in project',exact:true}).click();
 await expect(page.getByRole('button',{name:'Retry saved installation',exact:true})).toBeVisible();await page.getByRole('button',{name:'Retry saved installation',exact:true}).click();
 await expect.poll(()=>canvas.installationAdmissions.length).toBe(2);assert.deepEqual(canvas.installationAdmissions[0],canvas.installationAdmissions[1]);assert.equal(canvas.installations.size,1);
 await page.getByRole('checkbox',{name:'Select skill Fixture review',exact:true}).check();await page.getByLabel('Agent request',{exact:true}).fill('Use fixture skill');await page.getByRole('button',{name:'Start file task',exact:true}).click();await expect(page.getByText('Fixture result. No model or VM executed.',{exact:true})).toBeVisible();
 await page.getByRole('button',{name:'Remove skill Fixture review',exact:true}).click();await expect(page.locator('.cw-skill-selections')).toContainText('Inactive');assert.equal(canvas.tasks.size,1);assert.equal([...canvas.installations.values()][0].status,'removed');
 await page.getByLabel('Agent request',{exact:true}).fill('Rejected removed skill request');await page.getByRole('button',{name:'Start file task',exact:true}).click();await expect(page.getByRole('alert')).toContainText('Fixture selected skill is unavailable');
 await expect(page.getByRole('button',{name:'Deselect Fixture review',exact:true})).toBeEnabled();await page.getByRole('button',{name:'Deselect Fixture review',exact:true}).click();
 await page.getByRole('button',{name:'Start file task',exact:true}).click();await expect.poll(()=>canvas.tasks.size).toBe(2);assert.equal(canvas.admissions[1].body.selectedSkillIds,undefined);

});
