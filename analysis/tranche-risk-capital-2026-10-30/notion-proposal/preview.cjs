const fs = require('fs');
const path = require('path');
const {chromium} = require('playwright');
const sharp = require('sharp');
(async()=>{
  const root=__dirname;
  await sharp(path.join(root,'capital-and-losses.svg')).png().toFile(path.join(root,'capital-and-losses.png'));
  const browser=await chromium.launch({headless:true,...(process.env.CHROME_EXECUTABLE_PATH ? {executablePath:process.env.CHROME_EXECUTABLE_PATH} : {})});
  const page=await browser.newPage({viewport:{width:1180,height:960},deviceScaleFactor:2});
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto('file://'+path.join(root,'allocator.html'));
  const cases=[];
  for(const p of [0,10,30,47,50,100]){
    await page.locator('#junior').fill(String(p));
    const v=await page.locator('#yield').textContent();
    if(v!==(4+p*.12).toFixed(1))throw new Error('Incorrect yield at '+p+': '+v);
    cases.push({junior_percent:p,combined_apr:v});
  }
  await page.locator('[data-p="30"]').click();
  await page.locator('main').screenshot({path:path.join(root,'allocator-desktop.png')});
  await page.setViewportSize({width:390,height:844});
  const overflow=await page.evaluate(()=>document.documentElement.scrollWidth>window.innerWidth);
  if(overflow)throw new Error('Mobile horizontal overflow');
  await page.screenshot({path:path.join(root,'allocator-mobile.png'),fullPage:true});
  if(errors.length)throw new Error(errors.join('\n'));
  const prose=fs.readFileSync(path.join(root,'page.md'),'utf8');
  if(/[—–“”]/.test(prose))throw new Error('Prose punctuation requires humanizer pass');
  fs.writeFileSync(path.join(root,'verification.json'),JSON.stringify({cases,mobile_horizontal_overflow:overflow,browser_errors:errors,word_count:prose.split(/\s+/).length},null,2));
  await browser.close();
  console.log(JSON.stringify({cases,mobile_horizontal_overflow:overflow,browser_errors:errors}));
})();
