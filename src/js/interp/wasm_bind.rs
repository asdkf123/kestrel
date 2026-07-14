// WebAssembly ↔ JS 바인딩 (ArrayBuffer/메모리 동기화/instantiate).
use super::*;

impl Interp {
    // 네이티브 호출의 유일한 관문. DOM 을 바꾼 호출이면 여기서 MutationObserver
    // 배달을 한 번 예약한다 (호출부마다 예약하면 반드시 빠뜨린다).
    // 바이트열 → 진짜 ArrayBuffer (프렐류드의 __kArrayBuffer 로 만들어 프로토타입까지 맞춘다).
    pub(super) fn make_array_buffer(&mut self, bytes: &[u8]) -> Result<Value, String> {
        let ctor = env_get(&self.global, "__kArrayBuffer")
            .ok_or("__kArrayBuffer 가 프렐류드에 없다")?;
        let buf = self.construct(ctor, vec![Value::Num(bytes.len() as f64)])?;
        if let Value::Obj(o) = &buf {
            let arr = o.borrow().get("_b").cloned();
            if let Some(Value::Arr(a)) = arr {
                let mut items = a.borrow_mut();
                for (i, b) in bytes.iter().enumerate() {
                    items[i] = Value::Num(*b as f64);
                }
            }
        }
        Ok(buf)
    }

    // wasm 안에서 memory.grow 가 일어나면 선형 메모리는 **새 배열**로 바뀐다.
    // JS 쪽 Memory 객체의 buffer 는 그대로 옛 배열을 가리키므로, 다시 묶지 않으면
    // 그 뒤로 wasm 이 쓴 값이 JS 에 아예 보이지 않는다 (조용히 틀린다).
    // JS 가 메모리를 볼 수 있는 경계 — 호출이 돌아올 때, 임포트로 JS 를 부르기 직전 — 마다 부른다.
    pub(super) fn sync_wasm_memories(&mut self) {
        for i in 0..self.wasm_memories.len() {
            let (mem, obj) = self.wasm_memories[i].clone();
            let cur = mem.borrow().clone();
            let Value::Obj(o) = &obj else { continue };
            let buf = o.borrow().get("buffer").cloned();
            if let Some(Value::Obj(b)) = &buf {
                let same = matches!(
                    b.borrow().get("_b"),
                    Some(Value::Arr(a)) if Rc::ptr_eq(a, &cur)
                );
                if same {
                    continue;
                }
                // 표준: 커진 메모리의 옛 ArrayBuffer 는 분리된다 (byteLength → 0).
                let mut bm = b.borrow_mut();
                bm.insert("_b".to_string(), Value::Arr(ArrayObj::new(Vec::new())));
                bm.insert("byteLength".to_string(), Value::Num(0.0));
            }
            // 새 배열을 감싼 ArrayBuffer 로 갈아끼운다 (배열은 공유 — 사본이 아니다)
            let len = cur.borrow().len();
            let Some(ctor) = env_get(&self.global, "__kArrayBuffer") else { continue };
            let Ok(nb) = self.construct(ctor, vec![Value::Num(0.0)]) else { continue };
            if let Value::Obj(n) = &nb {
                let mut nm = n.borrow_mut();
                nm.insert("_b".to_string(), Value::Arr(cur));
                nm.insert("byteLength".to_string(), Value::Num(len as f64));
            }
            if let Value::Obj(o) = &obj {
                o.borrow_mut().insert("buffer".to_string(), nb);
            }
        }
    }

    // 모듈 + 임포트 → 인스턴스, 그리고 exports 객체.
    // mem_idx >= 0 이면 JS 가 미리 만든 WebAssembly.Memory 를 모듈 자신의 메모리로 쓴다.
    pub(super) fn wasm_instantiate(
        &mut self,
        mi: usize,
        imports: Value,
        mem_idx: f64,
    ) -> Result<Value, String> {
        use crate::wasm::{Extern, Export as WExport, ImportKind};
        let module = self
            .wasm_modules
            .get(mi)
            .cloned()
            .ok_or("wasm: 모듈 없음")?;

        // imports[모듈][이름] 조회
        let lookup = |me: &mut Self, m: &str, n: &str| -> Result<Value, String> {
            let ns = me.member_get(&imports, m)?;
            me.member_get(&ns, n)
        };

        let mut externs: Vec<Extern> = Vec::new();
        let mut import_fns: Vec<Value> = Vec::new();
        // 내보내진 memory 가 가리킬 JS Memory 객체 (임포트된 것이면 그것, 아니면 우리가 만든 것)
        let mut mem_obj = Value::Undefined;

        for imp in module.imports.clone() {
            let v = lookup(self, &imp.module, &imp.name).unwrap_or(Value::Undefined);
            match imp.kind {
                ImportKind::Func(_) => {
                    if matches!(v, Value::Undefined | Value::Null) {
                        return Err(format!(
                            "WebAssembly.LinkError: 임포트 {}.{} 가 없다",
                            imp.module, imp.name
                        ));
                    }
                    import_fns.push(v);
                    externs.push(Extern::Func);
                }
                ImportKind::Memory(min) => {
                    let idx = self.member_get(&v, "__mem")?;
                    let idx = match idx {
                        Value::Num(n) => n as usize,
                        _ => {
                            return Err(format!(
                                "WebAssembly.LinkError: 임포트 {}.{} 가 Memory 가 아니다",
                                imp.module, imp.name
                            ))
                        }
                    };
                    let (m, obj) = self
                        .wasm_memories
                        .get(idx)
                        .ok_or("wasm: 메모리 없음")?
                        .clone();
                    // 표준: 준 메모리가 모듈이 요구하는 최소 페이지보다 작으면 LinkError.
                    // 그냥 받으면 모듈이 없는 주소에 써서 조용히 죽는다.
                    let pages = m.borrow().borrow().len() / crate::wasm::PAGE;
                    if pages < min as usize {
                        return Err(format!(
                            "WebAssembly.LinkError: 임포트 {}.{} 메모리가 작다 ({} < {} 페이지)",
                            imp.module, imp.name, pages, min
                        ));
                    }
                    mem_obj = obj;
                    externs.push(Extern::Memory(m));
                }
                ImportKind::Global(t) => {
                    // 숫자로 오기도 하고 WebAssembly.Global 객체로 오기도 한다
                    let raw = match &v {
                        Value::Obj(_) => self.member_get(&v, "value")?,
                        other => other.clone(),
                    };
                    externs.push(Extern::Global(super::js_to_wasm_typed(&raw, t)));
                }
                ImportKind::Table => {
                    // 조용히 빈 테이블로 두면 call_indirect 가 엉뚱한 곳을 부른다.
                    return Err(
                        "WebAssembly.LinkError: 테이블 임포트는 아직 지원하지 않는다".to_string()
                    );
                }
            }
        }

        // 모듈 자신의 메모리 — JS 가 만든 버퍼를 그대로 쓴다 (살아있는 뷰)
        let own_mem = if mem_idx >= 0.0 {
            let (m, obj) = self
                .wasm_memories
                .get(mem_idx as usize)
                .ok_or("wasm: 메모리 없음")?
                .clone();
            mem_obj = obj;
            Some(m)
        } else {
            None
        };

        let inst = {
            let mut host = super::WasmHost {
                interp: self,
                imports: import_fns.clone(),
                module: module.clone(),
            };
            crate::wasm::instantiate(module.clone(), externs, own_mem, &mut host)?
        };
        let table_len = inst.table.borrow().len();
        self.sync_wasm_memories();
        self.wasm_instances
            .push(Rc::new(super::WasmInstance { inst, imports: import_fns }));
        let ii = (self.wasm_instances.len() - 1) as u32;

        // exports 객체
        let mut m = ObjMap::new();
        for (name, e) in &module.exports {
            let v = match e {
                WExport::Func(f) => Value::Native(Native::WasmCall(ii, *f)),
                WExport::Memory => mem_obj.clone(),
                // 내보내진 전역은 WebAssembly.Global 객체다 — .value 로 읽고 쓴다
                WExport::Global(g) => {
                    let mut go = ObjMap::new();
                    go.insert(
                        "value".to_string(),
                        Value::Accessor(Rc::new(AccessorPair {
                            get: Some(Value::Native(Native::WasmGlobalGet(ii, *g))),
                            set: Some(Value::Native(Native::WasmGlobalSet(ii, *g))),
                        })),
                    );
                    Value::Obj(Rc::new(RefCell::new(go)))
                }
                WExport::Table => {
                    let mut to = ObjMap::new();
                    to.insert("get".to_string(), Value::Native(Native::WasmTableGet(ii)));
                    to.insert("length".to_string(), Value::Num(table_len as f64));
                    Value::Obj(Rc::new(RefCell::new(to)))
                }
            };
            m.insert(name.clone(), v);
        }
        Ok(Value::Obj(Rc::new(RefCell::new(m))))
    }
}
