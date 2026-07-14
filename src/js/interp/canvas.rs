// <canvas> 2D 컨텍스트: 경로/도형/텍스트/이미지 그리기 명령.
use super::*;

// 컨텍스트의 __path 배열 조작 ([x0,y0,x1,y1,...] 평탄 저장).
pub(super) fn set_path(ctx: &Rc<RefCell<ObjMap>>, pts: Vec<Value>) {
    ctx.borrow_mut().insert("\u{0}path".to_string(), Value::Arr(ArrayObj::new(pts)));
}

pub(super) fn push_path(ctx: &Rc<RefCell<ObjMap>>, x: f32, y: f32) {
    if let Some(Value::Arr(a)) = ctx.borrow().get("\u{0}path") {
        a.borrow_mut().push(Value::Num(x as f64));
        a.borrow_mut().push(Value::Num(y as f64));
    }
}

pub(super) fn get_path(ctx: &Rc<RefCell<ObjMap>>) -> Vec<(f32, f32)> {
    if let Some(Value::Arr(a)) = ctx.borrow().get("\u{0}path") {
        let flat = a.borrow();
        return flat
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (to_num(&c[0]) as f32, to_num(&c[1]) as f32))
            .collect();
    }
    Vec::new()
}

// font 문자열에서 px 크기 추출 ("bold 16px sans-serif" → 16). 없으면 10.
pub(super) fn font_px(font: &str) -> f32 {
    for tok in font.split_whitespace() {
        if let Some(n) = tok.strip_suffix("px") {
            if let Ok(v) = n.parse::<f32>() {
                return v;
            }
        }
    }
    10.0
}

impl Interp {
    // canvas 2D 컨텍스트 메서드 처리. recv=컨텍스트 객체. ops 를 canvas_cmds 에 쌓는다.
    pub(super) fn canvas_method(
        &mut self,
        method: CanvasMethod,
        recv: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        use CanvasMethod::*;
        // addColorStop 의 수신자는 **그라디언트 객체**다 (컨텍스트가 아니다) — 먼저 처리.
        if matches!(method, AddColorStop) {
            if let Some(Value::Obj(g)) = &recv {
                let pos = args.first().map(to_num).unwrap_or(0.0);
                let col = args.get(1).map(to_display).unwrap_or_default();
                let entry = Value::Arr(ArrayObj::new(vec![Value::Num(pos), Value::Str(col)]));
                let stops = match g.borrow().get("\u{0}stops") {
                    Some(Value::Arr(st)) => Some(st.clone()),
                    _ => None,
                };
                if let Some(st) = stops {
                    st.borrow_mut().push(entry);
                }
            }
            return Ok(Value::Undefined);
        }
        let Some(Value::Obj(ctx)) = recv else { return Ok(Value::Undefined) };
        let canvas_id = match ctx.borrow().get("\u{0}canvas") {
            Some(Value::Num(n)) => *n as crate::dom::NodeId,
            _ => return Ok(Value::Undefined),
        };
        let num = |i: usize| args.get(i).map(to_num).unwrap_or(0.0) as f32;
        let style = |key: &str| -> crate::css::Color {
            match ctx.borrow().get(key) {
                Some(Value::Str(s)) => {
                    crate::css::parse_color(s).unwrap_or(crate::css::Color { r: 0, g: 0, b: 0, a: 255 })
                }
                _ => crate::css::Color { r: 0, g: 0, b: 0, a: 255 },
            }
        };
        // 그림자 상태를 op 스트림에 흘려보낸다 (캔버스는 상태 기계다).
        // shadowColor/Blur/OffsetX/Y 는 프로퍼티로 **있기만 하고 아무도 안 읽었다** —
        // 그림자를 지정해도 아무 일도 안 일어났다.
        {
            let sc = match ctx.borrow().get("shadowColor") {
                Some(Value::Str(s)) => crate::css::parse_color(s)
                    .unwrap_or(crate::css::Color { r: 0, g: 0, b: 0, a: 0 }),
                _ => crate::css::Color { r: 0, g: 0, b: 0, a: 0 },
            };
            let n = |k: &str| match ctx.borrow().get(k) {
                Some(Value::Num(v)) => *v as f32,
                _ => 0.0,
            };
            let next = CanvasOp::SetShadow {
                color: sc,
                blur: n("shadowBlur"),
                dx: n("shadowOffsetX"),
                dy: n("shadowOffsetY"),
            };
            let ops = self.canvas_cmds.entry(canvas_id).or_default();
            // 마지막으로 흘려보낸 상태와 같으면 다시 넣지 않는다
            let same = ops.iter().rev().find_map(|o| match o {
                CanvasOp::SetShadow { color, blur, dx, dy } => {
                    Some((*color, *blur, *dx, *dy))
                }
                _ => None,
            });
            let cur = match &next {
                CanvasOp::SetShadow { color, blur, dx, dy } => (*color, *blur, *dx, *dy),
                _ => unreachable!(),
            };
            if same != Some(cur) && (cur.0.a > 0 || same.is_some()) {
                ops.push(next);
            }
        }
        // 현재 변환 행렬(CTM). 캔버스는 상태 기계다 — translate/rotate/scale 이
        // 이후 그리기에 실제로 적용돼야 한다. 예전엔 전부 조용한 no-op 이라
        // 그림이 엉뚱한 자리에 그려지거나 사라졌다 (아무 말도 없이).
        let get_ctm = |ctx: &Rc<RefCell<ObjMap>>| -> crate::layout::Mat {
            match ctx.borrow().get("\u{0}ctm") {
                Some(Value::Arr(a)) => {
                    let v = a.borrow();
                    let g = |i: usize| v.get(i).map(to_num).unwrap_or(0.0) as f32;
                    crate::layout::Mat { a: g(0), b: g(1), c: g(2), d: g(3), e: g(4), f: g(5) }
                }
                _ => crate::layout::Mat::IDENTITY,
            }
        };
        let set_ctm = |ctx: &Rc<RefCell<ObjMap>>, m: crate::layout::Mat| {
            let v = vec![
                Value::Num(m.a as f64),
                Value::Num(m.b as f64),
                Value::Num(m.c as f64),
                Value::Num(m.d as f64),
                Value::Num(m.e as f64),
                Value::Num(m.f as f64),
            ];
            ctx.borrow_mut().insert("\u{0}ctm".to_string(), Value::Arr(ArrayObj::new(v)));
        };
        let alpha = |ctx: &Rc<RefCell<ObjMap>>| -> f32 {
            match ctx.borrow().get("globalAlpha") {
                Some(Value::Num(n)) => (*n as f32).clamp(0.0, 1.0),
                _ => 1.0,
            }
        };
        // globalAlpha 는 색의 알파에 곱해진다 (표준)
        let with_alpha = |c: crate::css::Color, a: f32| crate::css::Color {
            r: c.r,
            g: c.g,
            b: c.b,
            a: ((c.a as f32) * a).round().clamp(0.0, 255.0) as u8,
        };
        let font_px_of = |ctx: &Rc<RefCell<ObjMap>>| -> f32 {
            match ctx.borrow().get("font") {
                Some(Value::Str(f)) => font_px(f),
                _ => 10.0,
            }
        };
        // 텍스트 폭 (실제 폰트 메트릭). 폰트가 없으면 근사.
        let text_width = |text: &str, px: f32, ctx_fonts: Option<&crate::font::FontStack>| -> f32 {
            match ctx_fonts {
                Some(fonts) => text
                    .chars()
                    .map(|ch| {
                        let (fi, gid) = fonts.glyph_for(ch);
                        let f = fonts.font(fi);
                        f.advance_width(gid) as f32 * (px / f.units_per_em() as f32)
                    })
                    .sum(),
                None => text.chars().count() as f32 * px * 0.5,
            }
        };
        let fonts_ptr: Option<&crate::font::FontStack> =
            self.layout_ctx.as_ref().map(|c| unsafe { &*c.fonts });

        let a = alpha(&ctx);
        let cur_m = get_ctm(&ctx);

        // fillStyle/strokeStyle 이 그라디언트·패턴 객체면 그걸 쓴다 (문자열이면 색).
        // 예전엔 createLinearGradient 가 no-op 이라 그라디언트 채우기가 통째로 사라졌다.
        let paint_source = |ctx: &Rc<RefCell<ObjMap>>, key: &str| -> Option<Value> {
            match ctx.borrow().get(key) {
                Some(v @ Value::Obj(o)) if o.borrow().contains_key("\u{0}grad")
                    || o.borrow().contains_key("\u{0}pattern") =>
                {
                    Some(v.clone())
                }
                _ => None,
            }
        };
        // 그라디언트 객체 → (kind, stops)
        let grad_of = |v: &Value| -> Option<(crate::paint::CanvasGrad, Vec<(crate::css::Color, f32)>)> {
            let Value::Obj(o) = v else { return None };
            let b = o.borrow();
            let Some(Value::Arr(p)) = b.get("\u{0}grad") else { return None };
            let pv = p.borrow();
            let g = |i: usize| pv.get(i).map(to_num).unwrap_or(0.0) as f32;
            let radial = pv.len() >= 6;
            let kind = if radial {
                crate::paint::CanvasGrad::Radial {
                    x0: g(0), y0: g(1), r0: g(2), x1: g(3), y1: g(4), r1: g(5),
                }
            } else {
                crate::paint::CanvasGrad::Linear { x0: g(0), y0: g(1), x1: g(2), y1: g(3) }
            };
            let mut stops: Vec<(crate::css::Color, f32)> = Vec::new();
            if let Some(Value::Arr(st)) = b.get("\u{0}stops") {
                for e in st.borrow().iter() {
                    if let Value::Arr(pair) = e {
                        let pv = pair.borrow();
                        let pos = pv.first().map(to_num).unwrap_or(0.0) as f32;
                        let col = pv
                            .get(1)
                            .map(to_display)
                            .and_then(|s| crate::css::parse_color(&s))
                            .unwrap_or(crate::css::Color { r: 0, g: 0, b: 0, a: 255 });
                        stops.push((col, pos));
                    }
                }
            }
            stops.sort_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal));
            Some((kind, stops))
        };
        let pattern_of = |v: &Value| -> Option<(usize, bool)> {
            let Value::Obj(o) = v else { return None };
            let b = o.borrow();
            let Some(Value::Arr(p)) = b.get("\u{0}pattern") else { return None };
            let pv = p.borrow();
            let idx = pv.first().map(to_num)? as usize;
            let repeat = pv.get(1).map(to_bool).unwrap_or(true);
            Some((idx, repeat))
        };
        // 다각형의 경계 상자
        let bbox = |pts: &[(f32, f32)]| -> crate::layout::Rect {
            let (mut x0, mut y0, mut x1, mut y1) =
                (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
            for &(x, y) in pts {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            crate::layout::Rect { x: x0, y: y0, width: (x1 - x0).max(0.0), height: (y1 - y0).max(0.0) }
        };

        match method {
            // ── 그라디언트 / 패턴 객체 ──
            CreateLinearGradient | CreateRadialGradient => {
                let n = if matches!(method, CreateRadialGradient) { 6 } else { 4 };
                let params: Vec<Value> = (0..n).map(|i| Value::Num(num(i) as f64)).collect();
                let mut g = ObjMap::new();
                g.insert("\u{0}grad".to_string(), Value::Arr(ArrayObj::new(params)));
                g.insert("\u{0}stops".to_string(), Value::Arr(ArrayObj::new(Vec::new())));
                g.insert("addColorStop".to_string(), Value::Native(Native::Canvas(AddColorStop)));
                return Ok(Value::Obj(Rc::new(RefCell::new(g))));
            }
            AddColorStop => {}
            CreatePattern => {
                let src = match args.first() {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        match &dom.get(*id).node_type {
                            crate::dom::NodeType::Element(e) => e.attributes.get("src").cloned(),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let idx = src.as_ref().and_then(|raw| {
                    let abs = self.absolute_url(raw);
                    self.layout_ctx.as_ref().and_then(|c| unsafe {
                        (*c.img_map).get(&abs).or_else(|| (*c.img_map).get(raw)).map(|(i, _, _)| *i)
                    })
                });
                let Some(idx) = idx else {
                    self.canvas_warn("createPattern 의 소스를 찾지 못했다 (<img> 요소만 지원)");
                    return Ok(Value::Null);
                };
                let rep = args.get(1).map(to_display).unwrap_or_else(|| "repeat".to_string());
                let mut p = ObjMap::new();
                p.insert(
                    "\u{0}pattern".to_string(),
                    Value::Arr(ArrayObj::new(vec![
                        Value::Num(idx as f64),
                        Value::Bool(rep != "no-repeat"),
                    ])),
                );
                return Ok(Value::Obj(Rc::new(RefCell::new(p))));
            }
            // ── ImageData (실제 픽셀) ──
            // 캔버스의 픽셀을 읽으려면 **진짜로 그려 봐야** 한다. 지금까지의 명령을
            // 오프스크린으로 래스터화해서 그 영역을 잘라 준다.
            GetImageData => {
                let (sx, sy, sw, sh) = (num(0), num(1), num(2), num(3));
                let (sw, sh) = (sw.max(0.0) as usize, sh.max(0.0) as usize);
                if sw == 0 || sh == 0 {
                    return Ok(Value::Null);
                }
                let Some(lc) = self.layout_ctx.as_ref() else {
                    self.canvas_warn("getImageData 는 렌더 컨텍스트가 필요하다");
                    return Ok(Value::Null);
                };
                let (fonts, images) = unsafe { (&*lc.fonts, &*lc.images) };
                let ops = self.canvas_cmds.get(&canvas_id).cloned().unwrap_or_default();
                // 캔버스 크기
                let (cw, ch) = {
                    let dom = self.dom_arena()?;
                    match &dom.get(canvas_id).node_type {
                        crate::dom::NodeType::Element(e) => (
                            e.attributes
                                .get("width")
                                .and_then(|v| v.parse::<usize>().ok())
                                .unwrap_or(300),
                            e.attributes
                                .get("height")
                                .and_then(|v| v.parse::<usize>().ok())
                                .unwrap_or(150),
                        ),
                        _ => (300, 150),
                    }
                };
                let items = crate::window::canvas_items_at_origin(&ops, fonts);
                let img = crate::paint::rasterize_items(&items, cw, ch, fonts, images);
                let mut data: Vec<Value> = Vec::with_capacity(sw * sh * 4);
                for y in 0..sh {
                    for x in 0..sw {
                        let (px0, py0) = (sx as usize + x, sy as usize + y);
                        if px0 < cw && py0 < ch {
                            let o = (py0 * cw + px0) * 4;
                            for k in 0..4 {
                                data.push(Value::Num(img.rgba[o + k] as f64));
                            }
                        } else {
                            for _ in 0..4 {
                                data.push(Value::Num(0.0));
                            }
                        }
                    }
                }
                let mut m = ObjMap::new();
                m.insert("width".to_string(), Value::Num(sw as f64));
                m.insert("height".to_string(), Value::Num(sh as f64));
                m.insert("data".to_string(), Value::Arr(ArrayObj::new(data)));
                return Ok(Value::Obj(Rc::new(RefCell::new(m))));
            }
            CreateImageData => {
                let (w0, h0) = (num(0).max(0.0) as usize, num(1).max(0.0) as usize);
                let data: Vec<Value> = vec![Value::Num(0.0); w0 * h0 * 4];
                let mut m = ObjMap::new();
                m.insert("width".to_string(), Value::Num(w0 as f64));
                m.insert("height".to_string(), Value::Num(h0 as f64));
                m.insert("data".to_string(), Value::Arr(ArrayObj::new(data)));
                return Ok(Value::Obj(Rc::new(RefCell::new(m))));
            }
            PutImageData => {
                let Some(Value::Obj(d)) = args.first() else { return Ok(Value::Undefined) };
                let b = d.borrow();
                let w0 = b.get("width").map(to_num).unwrap_or(0.0) as usize;
                let h0 = b.get("height").map(to_num).unwrap_or(0.0) as usize;
                let Some(Value::Arr(px)) = b.get("data") else { return Ok(Value::Undefined) };
                let vals = px.borrow();
                if w0 == 0 || h0 == 0 || vals.len() < w0 * h0 * 4 {
                    return Ok(Value::Undefined);
                }
                let rgba: Vec<u8> = vals
                    .iter()
                    .take(w0 * h0 * 4)
                    .map(|v| to_num(v).clamp(0.0, 255.0) as u8)
                    .collect();
                drop(vals);
                drop(b);
                let img = std::rc::Rc::new(crate::png::Image { width: w0, height: h0, rgba });
                self.canvas_cmds.entry(canvas_id).or_default().push(CanvasOp::PutImage {
                    x: num(1),
                    y: num(2),
                    img,
                });
            }
            // ── 클립 ──
            Clip => {
                let pts = get_path(&ctx);
                if pts.len() < 3 {
                    self.canvas_warn("clip() 에 경로가 없다");
                    return Ok(Value::Undefined);
                }
                // 클립도 그리기 상태다 — save/restore 로 복원돼야 한다 (표준).
                let flat: Vec<Value> = pts
                    .iter()
                    .flat_map(|&(x, y)| [Value::Num(x as f64), Value::Num(y as f64)])
                    .collect();
                ctx.borrow_mut()
                    .insert("\u{0}clip".to_string(), Value::Arr(ArrayObj::new(flat)));
                self.canvas_cmds
                    .entry(canvas_id)
                    .or_default()
                    .push(CanvasOp::Clip { pts: Some(pts) });
            }
            // ── 곡선 ──
            BezierCurveTo | QuadraticCurveTo => {
                let path = get_path(&ctx);
                let Some(&(px0, py0)) = path.last() else {
                    return Ok(Value::Undefined); // 시작점이 없으면 무시 (표준)
                };
                let seg = 20;
                for k in 1..=seg {
                    let t = k as f32 / seg as f32;
                    let (x, y) = if matches!(method, BezierCurveTo) {
                        let (c1x, c1y, c2x, c2y, ex, ey) =
                            (num(0), num(1), num(2), num(3), num(4), num(5));
                        let u = 1.0 - t;
                        (
                            u * u * u * px0 + 3.0 * u * u * t * c1x + 3.0 * u * t * t * c2x + t * t * t * ex,
                            u * u * u * py0 + 3.0 * u * u * t * c1y + 3.0 * u * t * t * c2y + t * t * t * ey,
                        )
                    } else {
                        let (cx, cy, ex, ey) = (num(0), num(1), num(2), num(3));
                        let u = 1.0 - t;
                        (
                            u * u * px0 + 2.0 * u * t * cx + t * t * ex,
                            u * u * py0 + 2.0 * u * t * cy + t * t * ey,
                        )
                    };
                    push_path(&ctx, x, y);
                }
            }
            // ── 변환 상태 ──
            Translate | Rotate | Scale | Transform | SetTransform | ResetTransform => {
                use crate::layout::Mat;
                let m = match method {
                    Translate => Mat { e: num(0), f: num(1), ..Mat::IDENTITY },
                    Rotate => {
                        let t = num(0);
                        Mat { a: t.cos(), b: t.sin(), c: -t.sin(), d: t.cos(), e: 0.0, f: 0.0 }
                    }
                    Scale => Mat { a: num(0), d: num(1), ..Mat::IDENTITY },
                    _ => Mat {
                        a: num(0),
                        b: num(1),
                        c: num(2),
                        d: num(3),
                        e: num(4),
                        f: num(5),
                    },
                };
                let new_m = match method {
                    // setTransform/resetTransform 은 CTM 을 **대체**한다
                    SetTransform => m,
                    ResetTransform => Mat::IDENTITY,
                    // 나머지는 현재 CTM 에 **누적**된다 (새 변환이 먼저 적용)
                    _ => m.then(&cur_m),
                };
                set_ctm(&ctx, new_m);
                self.canvas_cmds
                    .entry(canvas_id)
                    .or_default()
                    .push(CanvasOp::SetTransform { m: new_m });
            }
            Save => {
                // 상태 전체를 스택에 (CTM + 스타일)
                let snap = vec![
                    ctx.borrow().get("\u{0}ctm").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("fillStyle").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("strokeStyle").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("lineWidth").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("font").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("globalAlpha").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("\u{0}clip").cloned().unwrap_or(Value::Null),
                ];
                let stack = match ctx.borrow().get("\u{0}stack") {
                    Some(Value::Arr(st)) => Some(st.clone()),
                    _ => None,
                };
                match stack {
                    Some(st) => st.borrow_mut().push(Value::Arr(ArrayObj::new(snap))),
                    None => {
                        ctx.borrow_mut().insert(
                            "\u{0}stack".to_string(),
                            Value::Arr(ArrayObj::new(vec![Value::Arr(ArrayObj::new(snap))])),
                        );
                    }
                }
            }
            Restore => {
                let popped = match ctx.borrow().get("\u{0}stack") {
                    Some(Value::Arr(st)) => st.borrow_mut().pop(),
                    _ => None,
                };
                if let Some(Value::Arr(snap)) = popped {
                    let v = snap.borrow().clone();
                    let keys = [
                        "\u{0}ctm",
                        "fillStyle",
                        "strokeStyle",
                        "lineWidth",
                        "font",
                        "globalAlpha",
                    ];
                    for (k, val) in keys.iter().zip(v.iter().cloned()) {
                        if !matches!(val, Value::Undefined) {
                            ctx.borrow_mut().insert(k.to_string(), val);
                        }
                    }
                    // 클립 복원 (Null 이면 클립 해제). 예전엔 restore 가 클립을 되돌리지
                    // 않아서, 그 뒤 그리기가 전부 옛 클립에 갇혀 사라졌다.
                    let saved_clip = v.get(6).cloned().unwrap_or(Value::Null);
                    let pts = match &saved_clip {
                        Value::Arr(a) => {
                            let f = a.borrow();
                            let mut out = Vec::new();
                            let mut i = 0;
                            while i + 1 < f.len() {
                                out.push((to_num(&f[i]) as f32, to_num(&f[i + 1]) as f32));
                                i += 2;
                            }
                            Some(out)
                        }
                        _ => None,
                    };
                    ctx.borrow_mut().insert("\u{0}clip".to_string(), saved_clip);
                    let m = get_ctm(&ctx);
                    let ops = self.canvas_cmds.entry(canvas_id).or_default();
                    ops.push(CanvasOp::SetTransform { m });
                    ops.push(CanvasOp::Clip { pts });
                }
            }
            // ── 측정 ──
            MeasureText => {
                let text = args.first().map(to_display).unwrap_or_default();
                let px = font_px_of(&ctx);
                let w = text_width(&text, px, fonts_ptr);
                let mut m = ObjMap::new();
                m.insert("width".to_string(), Value::Num(w as f64));
                m.insert("actualBoundingBoxAscent".to_string(), Value::Num((px * 0.8) as f64));
                m.insert("actualBoundingBoxDescent".to_string(), Value::Num((px * 0.2) as f64));
                return Ok(Value::Obj(Rc::new(RefCell::new(m))));
            }
            // ── 이미지 ──
            DrawImage => {
                // drawImage(img, dx, dy [, dw, dh]) — <img> 요소만 지원 (캔버스 소스는 미지원).
                // 이미지 맵은 src(절대 URL) → (인덱스, 폭, 높이) 다.
                let src = match args.first() {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        match &dom.get(*id).node_type {
                            crate::dom::NodeType::Element(e) => e.attributes.get("src").cloned(),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let idx = src.as_ref().and_then(|raw| {
                    let abs = self.absolute_url(raw);
                    self.layout_ctx.as_ref().and_then(|c| unsafe {
                        (*c.img_map)
                            .get(&abs)
                            .or_else(|| (*c.img_map).get(raw))
                            .map(|(i, _, _)| *i)
                    })
                });
                let Some(idx) = idx else {
                    self.canvas_warn("drawImage 의 소스를 찾지 못했다 (<img> 요소만 지원)");
                    return Ok(Value::Undefined);
                };
                let (dx, dy) = (num(1), num(2));
                let (dw, dh) = if args.len() >= 5 {
                    (num(3), num(4))
                } else {
                    (0.0, 0.0) // 0 이면 호스트가 고유 크기로 그린다
                };
                self.canvas_cmds
                    .entry(canvas_id)
                    .or_default()
                    .push(CanvasOp::DrawImage { idx, x: dx, y: dy, w: dw, h: dh });
            }
            // ── 경로 ──
            Ellipse => {
                // ellipse(cx, cy, rx, ry, rot, start, end)
                let (cx, cy, rx, ry) = (num(0), num(1), num(2), num(3));
                let rot = num(4);
                let (s, e) = (num(5), num(6));
                for k in 0..=32 {
                    let t = s + (e - s) * k as f32 / 32.0;
                    let (px0, py0) = (rx * t.cos(), ry * t.sin());
                    let x = cx + px0 * rot.cos() - py0 * rot.sin();
                    let y = cy + px0 * rot.sin() + py0 * rot.cos();
                    push_path(&ctx, x, y);
                }
            }
            RoundRect => {
                let (x, y, w, h) = (num(0), num(1), num(2), num(3));
                let r = args.get(4).map(to_num).unwrap_or(0.0) as f32;
                let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
                let corner = |cx: f32, cy: f32, a0: f32, a1: f32, out: &mut Vec<(f32, f32)>| {
                    for k in 0..=6 {
                        let t = a0 + (a1 - a0) * k as f32 / 6.0;
                        out.push((cx + r * t.cos(), cy + r * t.sin()));
                    }
                };
                let mut pts = Vec::new();
                use std::f32::consts::PI;
                corner(x + w - r, y + r, -PI / 2.0, 0.0, &mut pts);
                corner(x + w - r, y + h - r, 0.0, PI / 2.0, &mut pts);
                corner(x + r, y + h - r, PI / 2.0, PI, &mut pts);
                corner(x + r, y + r, PI, 1.5 * PI, &mut pts);
                for (px0, py0) in pts {
                    push_path(&ctx, px0, py0);
                }
            }
            FillRect => {
                let rect = crate::layout::Rect {
                    x: num(0),
                    y: num(1),
                    width: num(2),
                    height: num(3),
                };
                let src = paint_source(&ctx, "fillStyle");
                let op = match src.as_ref().and_then(|v| grad_of(v)) {
                    Some((kind, stops)) => CanvasOp::FillGradient { rect, shape: None, kind, stops },
                    None => match src.as_ref().and_then(|v| pattern_of(v)) {
                        Some((idx, repeat)) => {
                            CanvasOp::FillPattern { rect, shape: None, idx, repeat }
                        }
                        None => CanvasOp::FillRect {
                            x: rect.x,
                            y: rect.y,
                            w: rect.width,
                            h: rect.height,
                            color: with_alpha(style("fillStyle"), a),
                        },
                    },
                };
                self.canvas_cmds.entry(canvas_id).or_default().push(op);
            }
            ClearRect => self
                .canvas_cmds
                .entry(canvas_id)
                .or_default()
                .push(CanvasOp::ClearRect { x: num(0), y: num(1), w: num(2), h: num(3) }),
            StrokeRect => {
                let lw = match ctx.borrow().get("lineWidth") {
                    Some(Value::Num(n)) => *n as f32,
                    _ => 1.0,
                };
                self.canvas_cmds.entry(canvas_id).or_default().push(CanvasOp::StrokeRect {
                    x: num(0),
                    y: num(1),
                    w: num(2),
                    h: num(3),
                    color: with_alpha(style("strokeStyle"), a),
                    lw,
                });
            }
            FillText => {
                let text = args.first().map(to_display).unwrap_or_default();
                let px = font_px_of(&ctx);
                // textAlign/textBaseline 을 실제로 반영한다 (표준). 예전엔 속성 자체가 없어
                // 가운데 정렬한 텍스트가 왼쪽으로 밀렸다.
                let w = text_width(&text, px, fonts_ptr);
                let align = match ctx.borrow().get("textAlign") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => "start".to_string(),
                };
                let dx = match align.as_str() {
                    "center" => -w / 2.0,
                    "right" | "end" => -w,
                    _ => 0.0,
                };
                let baseline = match ctx.borrow().get("textBaseline") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => "alphabetic".to_string(),
                };
                let dy = match baseline.as_str() {
                    "top" | "hanging" => px * 0.8,
                    "middle" => px * 0.3,
                    "bottom" | "ideographic" => -px * 0.2,
                    _ => 0.0,
                };
                self.canvas_cmds.entry(canvas_id).or_default().push(CanvasOp::FillText {
                    text,
                    x: num(0) + dx,
                    y: num(1) + dy,
                    color: with_alpha(style("fillStyle"), a),
                    px,
                });
            }
            // 경로: __path 에 점을 쌓았다가 fill/stroke 시 폴리곤으로.
            BeginPath => set_path(&ctx, Vec::new()),
            MoveTo | LineTo => push_path(&ctx, num(0), num(1)),
            Rect => {
                let (x, y, w, h) = (num(0), num(1), num(2), num(3));
                for (px0, py0) in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
                    push_path(&ctx, px0, py0);
                }
            }
            Arc => {
                let (cx, cy, r) = (num(0), num(1), num(2));
                let (s, e) = (num(3), num(4));
                let seg = 24;
                for k in 0..=seg {
                    let t = s + (e - s) * k as f32 / seg as f32;
                    push_path(&ctx, cx + r * t.cos(), cy + r * t.sin());
                }
            }
            ClosePath => {}
            Fill => {
                let pts = get_path(&ctx);
                if pts.len() >= 3 {
                    let src = paint_source(&ctx, "fillStyle");
                    let rect = bbox(&pts);
                    let op = match src.as_ref().and_then(|v| grad_of(v)) {
                        Some((kind, stops)) => {
                            CanvasOp::FillGradient { rect, shape: Some(pts), kind, stops }
                        }
                        None => match src.as_ref().and_then(|v| pattern_of(v)) {
                            Some((idx, repeat)) => {
                                CanvasOp::FillPattern { rect, shape: Some(pts), idx, repeat }
                            }
                            None => CanvasOp::FillPath {
                                pts,
                                color: with_alpha(style("fillStyle"), a),
                            },
                        },
                    };
                    self.canvas_cmds.entry(canvas_id).or_default().push(op);
                }
            }
            // 경로 스트로크: 각 선분을 두께만큼의 사각형(폴리곤)으로 그린다.
            // 예전엔 통째로 무시돼서 stroke() 한 그림이 아예 안 나왔다.
            Stroke => {
                let pts = get_path(&ctx);
                let lw = match ctx.borrow().get("lineWidth") {
                    Some(Value::Num(n)) => (*n as f32).max(1.0),
                    _ => 1.0,
                };
                // lineCap/lineJoin 은 프로퍼티로 **있기만 하고 아무도 안 읽었다**
                // (round 캡을 지정해도 butt 로 나왔다 — 속성은 있는데 아무 일도 안 하는 거짓말).
                let cap = match ctx.borrow().get("lineCap") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => "butt".to_string(),
                };
                let join = match ctx.borrow().get("lineJoin") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => "miter".to_string(),
                };
                let color = with_alpha(style("strokeStyle"), a);
                let ops = self.canvas_cmds.entry(canvas_id).or_default();
                let n = pts.len();
                for (i, w) in pts.windows(2).enumerate() {
                    let ((mut x0, mut y0), (mut x1, mut y1)) = (w[0], w[1]);
                    let (dx, dy) = (x1 - x0, y1 - y0);
                    let len = (dx * dx + dy * dy).sqrt();
                    if len < 0.01 {
                        continue;
                    }
                    let (ux, uy) = (dx / len, dy / len);
                    // square 캡: 끝을 반두께만큼 연장 (표준)
                    if cap == "square" {
                        if i == 0 {
                            x0 -= ux * lw / 2.0;
                            y0 -= uy * lw / 2.0;
                        }
                        if i + 2 == n {
                            x1 += ux * lw / 2.0;
                            y1 += uy * lw / 2.0;
                        }
                    }
                    let (nx, ny) = (-uy * lw / 2.0, ux * lw / 2.0);
                    ops.push(CanvasOp::FillPath {
                        pts: vec![
                            (x0 + nx, y0 + ny),
                            (x1 + nx, y1 + ny),
                            (x1 - nx, y1 - ny),
                            (x0 - nx, y0 - ny),
                        ],
                        color,
                    });
                    // round 캡/조인: 끝점/이음새에 반지름 lw/2 의 원을 얹는다
                    let circle = |cx: f32, cy: f32| -> Vec<(f32, f32)> {
                        (0..16)
                            .map(|k| {
                                let t = k as f32 / 16.0 * std::f32::consts::TAU;
                                (cx + t.cos() * lw / 2.0, cy + t.sin() * lw / 2.0)
                            })
                            .collect()
                    };
                    if cap == "round" {
                        if i == 0 {
                            ops.push(CanvasOp::FillPath { pts: circle(w[0].0, w[0].1), color });
                        }
                        if i + 2 == n {
                            ops.push(CanvasOp::FillPath { pts: circle(w[1].0, w[1].1), color });
                        }
                    }
                    // 이음새 (마지막 선분 제외). 아무것도 안 하면 바깥쪽에 V 자 홈이 남는다.
                    if i + 2 < n {
                        let (jx, jy) = w[1];
                        let (x2, y2) = pts[i + 2];
                        let (dx2, dy2) = (x2 - jx, y2 - jy);
                        let l2 = (dx2 * dx2 + dy2 * dy2).sqrt();
                        if l2 < 0.01 {
                            continue;
                        }
                        let (ux2, uy2) = (dx2 / l2, dy2 / l2);
                        match join.as_str() {
                            "round" => ops.push(CanvasOp::FillPath { pts: circle(jx, jy), color }),
                            _ => {
                                // 바깥쪽 코너 두 점 (양쪽 다 채워도 안쪽은 이미 덮여 있어 무해)
                                let (n1x, n1y) = (-uy * lw / 2.0, ux * lw / 2.0);
                                let (n2x, n2y) = (-uy2 * lw / 2.0, ux2 * lw / 2.0);
                                for sgn in [1.0f32, -1.0] {
                                    let a1 = (jx + n1x * sgn, jy + n1y * sgn);
                                    let a2 = (jx + n2x * sgn, jy + n2y * sgn);
                                    // 마이터 점 = 두 오프셋 선의 교점 (평행이면 없음)
                                    let cross = ux * uy2 - uy * ux2;
                                    let mut poly = vec![(jx, jy), a1, a2];
                                    if join == "miter" && cross.abs() > 1e-4 {
                                        // a1 + t·u = a2 + s·u2 를 풀어 교점
                                        let t = ((a2.0 - a1.0) * uy2 - (a2.1 - a1.1) * ux2) / cross;
                                        let m = (a1.0 + ux * t, a1.1 + uy * t);
                                        // 마이터 길이 한계 (기본 10) — 넘으면 bevel 로
                                        let ml = ((m.0 - jx).powi(2) + (m.1 - jy).powi(2)).sqrt()
                                            / (lw / 2.0);
                                        if ml <= 10.0 {
                                            poly = vec![(jx, jy), a1, m, a2];
                                        }
                                    }
                                    ops.push(CanvasOp::FillPath { pts: poly, color });
                                }
                            }
                        }
                    }
                }
            }
            Noop => {}
        }
        Ok(Value::Undefined)
    }

    // 캔버스 미지원 기능 경고 (같은 메시지는 한 번만)
    pub(super) fn canvas_warn(&mut self, msg: &str) {
        if self.canvas_warned.insert(msg.to_string()) {
            println!("[canvas] {}", msg);
        }
    }
}
