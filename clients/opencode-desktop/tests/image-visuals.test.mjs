import { test } from "node:test"
import assert from "node:assert/strict"
import { Window } from "happy-dom"
import { shapeNames, shapePoint, particleFrames, animateParticles, openImageViewer, imageCss } from "../src/image-visuals.mjs"

test("ten distinct finite shapes, one-second holds and a seamless scattered loop", () => {
  assert.equal(shapeNames.length, 10)
  const shapes = shapeNames.map((_,shape) => Array.from({length:72},(_,i) => shapePoint(shape,i/72)))
  assert.equal(new Set(shapes.map(JSON.stringify)).size, 10)
  assert.ok(shapes.flat(2).every(Number.isFinite))
  for (let i=0;i<72;i++) {
    const frames=particleFrames(i)
    assert.equal(frames[0].transform,frames.at(-1).transform)
    assert.equal(frames[0].opacity,frames.at(-1).opacity)
    for(let cycle=0;cycle<10;cycle++) {
      const a=frames[cycle*4+1], b=frames[cycle*4+2]
      assert.equal(a.transform,b.transform)
      assert.ok(Math.abs((b.offset-a.offset)*70000-1000)<.001)
    }
    assert.ok(frames.every((f,i) => i===0 || f.offset>=frames[i-1].offset))
  }
  assert.match(imageCss,/width:300px;max-width:100%/)
  assert.doesNotMatch(imageCss,/background-position/)
})

test("particle animation pauses for reduced motion and releases every animation on disposal", () => {
  const window=new Window(), doc=window.document, box=doc.createElement("div")
  doc.body.append(box)
  const animations=[]
  window.HTMLElement.prototype.animate=function(frames, options) {
    const a={paused:false,cancelled:false,currentTime:0,pause(){this.paused=true},play(){this.paused=false},cancel(){this.cancelled=true}}
    animations.push(a);return a
  }
  const media=new window.EventTarget();media.matches=true
  window.matchMedia=()=>media
  const cleanup=animateParticles(box,doc)
  assert.equal(box.querySelectorAll("i").length,72)
  assert.ok(animations.every(a=>a.paused && a.currentTime===2800))
  media.matches=false;media.dispatchEvent(new window.Event("change"))
  assert.ok(animations.every(a=>!a.paused))
  cleanup()
  assert.ok(animations.every(a=>a.cancelled))
  assert.equal(box.children.length,0)
})

test("fullscreen dialog supports buttons, wheel, keyboard, fit, drag and focus restoration", () => {
  const window=new Window(), doc=window.document
  const trigger=doc.createElement("button");doc.body.append(trigger);trigger.focus()
  const close=openImageViewer({uri:"data:image/png;base64,iVBORw0KGgo=",name:"fixture.png"},doc)
  const dialog=doc.querySelector("dialog"), stage=dialog.querySelector(".exr-image-stage"), output=dialog.querySelector("output"), img=dialog.querySelector("img")
  stage.getBoundingClientRect=()=>({left:0,top:0,width:800,height:600})
  assert.equal(dialog.open,true)
  assert.equal(output.textContent,"100%")
  dialog.querySelector('[aria-label="Zoom in"]').click();assert.equal(output.textContent,"125%")
  const wheel = new window.WheelEvent("wheel",{deltaY:-100,cancelable:true})
  Object.defineProperties(wheel, { clientX: { value:400 }, clientY: { value:300 } })
  stage.dispatchEvent(wheel)
  assert.ok(parseInt(output.textContent)>125)
  stage.onpointerdown({button:0,pointerId:1,clientX:100,clientY:100})
  stage.onpointermove({pointerId:1,clientX:120,clientY:130})
  assert.match(img.style.transform,/translate\(20px,30px\)/)
  stage.onpointerup({pointerId:1})
  dialog.dispatchEvent(new window.KeyboardEvent("keydown",{key:"0"}));assert.equal(output.textContent,"100%")
  stage.onpointerdown({button:0,pointerId:1,clientX:300,clientY:300})
  stage.onpointerdown({button:0,pointerId:2,clientX:500,clientY:300})
  stage.onpointermove({pointerId:2,clientX:700,clientY:300})
  assert.equal(output.textContent,"200%")
  stage.onpointercancel({pointerId:1});stage.onlostpointercapture({pointerId:2})
  dialog.dispatchEvent(new window.KeyboardEvent("keydown",{key:"0"}));assert.equal(output.textContent,"100%")
  dialog.dispatchEvent(new window.KeyboardEvent("keydown",{key:"-"}));assert.equal(output.textContent,"80%")
  dialog.querySelector('[aria-label="Fit"]').click();assert.equal(output.textContent,"100%")
  dialog.dispatchEvent(new window.Event("cancel",{cancelable:true}))
  assert.equal(doc.querySelector("dialog"),null)
  assert.equal(doc.activeElement,trigger)
  close()
})
