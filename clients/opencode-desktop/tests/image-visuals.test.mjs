import { test } from "node:test"
import assert from "node:assert/strict"
import { Window } from "happy-dom"
import { shapeNames, shapePoint, particleFrames, particleTiming, animateParticles, openImageViewer, imageCss } from "../src/image-visuals.mjs"

test("eight distinct finite shapes morph directly and quickly, with one-second holds and a seamless loop", () => {
  assert.equal(shapeNames.length, 8)
  assert.equal(shapeNames.includes("heart"),false)
  assert.equal(shapeNames.includes("flower"),false)
  const shapes = shapeNames.map((_,shape) => Array.from({length:72},(_,i) => shapePoint(shape,i/72)))
  assert.equal(new Set(shapes.map(JSON.stringify)).size, shapeNames.length)
  assert.ok(shapes.flat(2).every(Number.isFinite))
  for (let i=0;i<72;i++) {
    const frames=particleFrames(i)
    assert.equal(frames.length,shapeNames.length*2+1, "only figure holds and the closing figure; no scatter frames")
    assert.equal(frames[0].transform,frames.at(-1).transform)
    assert.equal(frames[0].opacity,frames.at(-1).opacity)
    const duration=shapeNames.length*(particleTiming.hold+particleTiming.transition)
    for(let cycle=0;cycle<shapeNames.length;cycle++) {
      const a=frames[cycle*2], b=frames[cycle*2+1]
      assert.equal(a.transform,b.transform)
      const [x,y] = shapePoint((cycle*7)%shapeNames.length,i/72)
      assert.equal(a.transform,`translate(${x}px, ${y}px)`)
      assert.ok(Math.abs((b.offset-a.offset)*duration-1000)<.001)
      assert.ok(Math.abs((frames[cycle*2+2].offset-b.offset)*duration-1400)<.001)
    }
    assert.ok(frames.every((f,i) => i===0 || f.offset>=frames[i-1].offset))
  }
  assert.match(imageCss,/width:300px;max-width:100%/)
  assert.match(imageCss,/height:auto!important/)
  assert.match(imageCss,/max-height:300px!important/)
  assert.match(imageCss,/margin:0!important/)
  assert.match(imageCss,/background-size:calc\(100% \/ 3\) 100%/)
  assert.match(imageCss,/exr-image-shimmer 1.8s linear infinite/)
  assert.match(imageCss,/translate3d\(calc\(100% \/ 3\),0,0\)/)
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
  assert.ok(animations.every(a=>a.paused && a.currentTime===0))
  media.matches=false;media.dispatchEvent(new window.Event("change"))
  assert.ok(animations.every(a=>!a.paused))
  cleanup()
  assert.ok(animations.every(a=>a.cancelled))
  assert.equal(box.children.length,0)
})

test("fullscreen dialog has no visible controls, and supports wheel, keyboard, fit, drag and focus restoration", () => {
  const window=new Window(), doc=window.document
  const trigger=doc.createElement("button");doc.body.append(trigger);trigger.focus()
  const close=openImageViewer({uri:"data:image/png;base64,iVBORw0KGgo=",name:"fixture.png"},doc)
  const dialog=doc.querySelector("dialog"), stage=dialog.querySelector(".exr-image-stage"), img=dialog.querySelector("img")
  stage.getBoundingClientRect=()=>({left:0,top:0,width:800,height:600})
  assert.equal(dialog.open,true)
  assert.equal(dialog.querySelector("button,output"),null)
  assert.equal(dialog.textContent,"")
  assert.equal(doc.activeElement,dialog)
  assert.match(img.style.transform,/scale\(1\)/)
  dialog.dispatchEvent(new window.KeyboardEvent("keydown",{key:"+"}));assert.match(img.style.transform,/scale\(1.25\)/)
  const wheel = new window.WheelEvent("wheel",{deltaY:-100,cancelable:true})
  Object.defineProperties(wheel, { clientX: { value:400 }, clientY: { value:300 } })
  stage.dispatchEvent(wheel)
  assert.ok(Number(/scale\((.+)\)/.exec(img.style.transform)[1])>1.25)
  stage.onpointerdown({button:0,pointerId:1,clientX:100,clientY:100})
  stage.onpointermove({pointerId:1,clientX:120,clientY:130})
  assert.match(img.style.transform,/translate\(20px,30px\)/)
  stage.onpointerup({pointerId:1})
  dialog.dispatchEvent(new window.KeyboardEvent("keydown",{key:"0"}));assert.match(img.style.transform,/scale\(1\)/)
  stage.onpointerdown({button:0,pointerId:1,clientX:300,clientY:300})
  stage.onpointerdown({button:0,pointerId:2,clientX:500,clientY:300})
  stage.onpointermove({pointerId:2,clientX:700,clientY:300})
  assert.match(img.style.transform,/scale\(2\)/)
  stage.onpointercancel({pointerId:1});stage.onlostpointercapture({pointerId:2})
  dialog.dispatchEvent(new window.KeyboardEvent("keydown",{key:"0"}));assert.match(img.style.transform,/scale\(1\)/)
  dialog.dispatchEvent(new window.KeyboardEvent("keydown",{key:"-"}));assert.match(img.style.transform,/scale\(0.8\)/)
  stage.ondblclick();assert.match(img.style.transform,/scale\(2\)/)
  stage.ondblclick();assert.match(img.style.transform,/scale\(1\)/)
  dialog.dispatchEvent(new window.Event("cancel",{cancelable:true}))
  assert.equal(doc.querySelector("dialog"),null)
  assert.equal(doc.activeElement,trigger)
  close()
})
