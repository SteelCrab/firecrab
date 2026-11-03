// Synthetic source and installation controller. No download, provider or VM executes.
import { installCanvasAgentFixture } from './canvas-agent-fixture.mjs';
export function installCanvasSkillsFixture(fixture) {
 const canvas=installCanvasAgentFixture(fixture),installations=new Map(),installationAdmissions=[];
 const entries=[
  {source:'openai',kind:'skill',path:'plugins/fixture/skills/review',name:'Fixture review',revision:'a'.repeat(40),instructions:['SKILL.md']},
  {source:'openai',kind:'skill_bundle',path:'plugins/fixture',name:'Fixture bundle',revision:'a'.repeat(40),instructions:['skills/review/SKILL.md']},
  {source:'anthropic',kind:'skill',path:'skills/fixture-design',name:'Fixture design',revision:'b'.repeat(40),instructions:['SKILL.md']},
 ];
 const catalogs=['openai','anthropic'].map(source=>({source,url:source==='openai'?'https://github.com/openai/plugins':'https://github.com/anthropics/skills',entries:[]}));
 const path=canvas.base+'/skills';let lose=false;
 fixture.handlers.set('GET '+path+'/catalog',({send})=>send(200,catalogs));
 fixture.handlers.set('GET '+path+'/installations',({send})=>send(200,[...installations.values()]));
 for(const source of ['openai','anthropic'])fixture.handlers.set('POST '+path+'/catalog/'+source,({send})=>{const catalog=catalogs.find(c=>c.source===source);catalog.entries=entries.filter(e=>e.source===source);catalog.revision=catalog.entries[0].revision;return send(200,catalog);});
 const inherited=fixture.matchHandler;
 fixture.matchHandler=({method,path:target})=>{
  if(method==='PUT'&&target.startsWith(`/api/ai-sandbox/projects/${canvas.project}/tasks/`))return input=>{
   if(input.body.selectedSkillIds?.some(id=>installations.get(id)?.status!=='installed'))return input.send(409,{error:{code:'workspace_conflict',message:'Fixture selected skill is unavailable'}});
   return inherited({method,path:target})(input);
  };
  if(target.startsWith(path+'/installations/'))return ({send,body})=>{
   const id=target.split('/').at(-1);let row=installations.get(id);
   if(method==='PUT'){
    const entry=entries.find(e=>e.source===body.source&&e.path===body.path&&e.revision===body.revision&&e.kind===body.kind);
    if(!entry)return send(409,{error:{message:'Unknown synthetic source'}});
    installationAdmissions.push({id,body:structuredClone(body)});
    if(!row){row={id,entry,root:'.agents/firecrab-skills/'+id,status:'installed',files:[{path:'SKILL.md',version:12,operationId:id}],sha256:'c'.repeat(64)};installations.set(id,row);}
    if(lose){lose=false;return send(503,{error:{message:'Fixture installation acknowledgement lost'}});}
   }
   if(!row)return send(404,{error:{code:'not_found',message:'No fixture installation'}});
   if(method==='DELETE')row.status='removed';return send(200,row);
  };
  return inherited?.({method,path:target});
 };
 return {...canvas,entries,catalogs,installations,installationAdmissions,loseNextInstallation(){lose=true;}};
}
