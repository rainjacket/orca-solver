// Run with node --test cli/tests/browser.test.js. No browser dependencies.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(require('node:path').join(__dirname, '../src/browser/browser.js'), 'utf8');
function browser({saved=null, failStorage=false} = {}) {
  const elements = new Map();
  function element(id) {
    if (!elements.has(id)) elements.set(id, {
      children: [], handlers: {}, textContent: '', disabled: false,
      addEventListener(event, fn) { this.handlers[event] = fn; },
      appendChild(child) { this.children.push(child); },
      set innerHTML(value) { this.children = []; },
      dataset: {},
    });
    return elements.get(id);
  }
  let stored = saved;
  // Pairwise-distant grids produce >100 groups without an expensive random fixture.
  const solutions = Array.from({length:130}, (_, n) => {
    const a = String.fromCharCode(65 + Math.floor(n / 26));
    const b = String.fromCharCode(65 + n % 26);
    return (a.repeat(5) + b.repeat(5) + '\n').repeat(10);
  });
  vm.runInNewContext(source, {
    SOLUTIONS: solutions, GRID_ROWS:10, GRID_COLS:10,
    location:{pathname:'/test.html'},
    document:{getElementById:element, createElement:() => ({dataset:{}})},
    localStorage:{getItem:() => stored, setItem:(_,v) => { if(failStorage) throw Error('quota'); stored=v; },removeItem:() => {stored=null;}},
  });
  return {element, solutions, stored:() => stored};
}
test('pagination bounds rendered grids and reaches remaining groups', () => {
  const b = browser();
  assert.equal(b.element('groups').children.length,100);
  b.element('next-page').handlers.click();
  assert.equal(b.element('groups').children.length,30);
  assert.equal(b.element('next-page').disabled,true);
  b.element('prev-page').handlers.click();
  assert.equal(b.element('groups').children.length,100);
});
test('storage failure does not prevent mark rendering', () => {
  const b=browser({failStorage:true});
  b.element('groups').handlers.click({target:{closest:() => ({dataset:{group:'0',mark:'good'}})}});
  assert.match(b.element('summary').textContent,/1 kept/);
  assert.match(b.element('storage-status').textContent,/Export/);
});
test('legacy marks load and writes use compact IDs', () => {
  const first = 'A'.repeat(10)+'\n';
  const b=browser({saved:JSON.stringify([[first.repeat(10),'good']])});
  assert.match(b.element('summary').textContent,/1 kept/);
  b.element('groups').handlers.click({target:{closest:() => ({dataset:{group:'1',mark:'bad'}})}});
  const saved=JSON.parse(b.stored());
  assert.equal(saved.version,2);
  assert.equal(typeof saved.marks[0][0],'number');
});
test('import merges portable marks and rejects malformed input', async () => {
  const b=browser();
  const handler=b.element('import-marks').handlers.change;
  await handler({target:{files:[{text:async()=>JSON.stringify([[b.solutions[0],'bad']])}],value:'file'}});
  assert.match(b.element('summary').textContent,/1 skipped/);
  await handler({target:{files:[{text:async()=>'{}'}],value:'file'}});
  assert.match(b.element('storage-status').textContent,/Could not import/);
});
