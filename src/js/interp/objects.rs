// JS 객체 모델: Value 와 그 구성 타입들 (ObjMap/ArrayObj/JsFn/JsClass/Instance).
// 프로퍼티 순서는 삽입 순서를 보존한다 (표준의 OrdinaryOwnPropertyKeys).
use super::*;

// 정규 배열 인덱스인가 (0 ~ 2^32-2, 선행 0 없음). 열거 순서 결정에 쓰인다.
pub fn array_index(k: &str) -> Option<u32> {
    if k.is_empty() || (k.len() > 1 && k.starts_with('0')) {
        return None;
    }
    match k.parse::<u32>() {
        Ok(n) if n != u32::MAX => Some(n),
        _ => None,
    }
}

// 삽입 순서를 유지하는 객체 프로퍼티 맵 (ECMAScript OrdinaryOwnPropertyKeys):
// 정수 인덱스 키는 오름차순으로 먼저, 그다음 문자열 키는 삽입 순서.
// HashMap 과 같은 메서드 이름을 노출해 호출부를 그대로 둔다.
#[derive(Clone, Debug, Default)]
pub struct ObjMap {
    entries: Vec<(String, Value)>,
    index: HashMap<String, usize>,
}

impl ObjMap {
    pub fn new() -> ObjMap {
        ObjMap::default()
    }
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.index.get(k).map(|&i| &self.entries[i].1)
    }
    pub fn contains_key(&self, k: &str) -> bool {
        self.index.contains_key(k)
    }
    // 정수 인덱스 키는 정렬 위치에, 문자열 키는 끝에 삽입할 위치를 구한다.
    pub fn insert_position(&self, k: &str) -> usize {
        match array_index(k) {
            Some(kn) => {
                let mut pos = 0;
                for (ek, _) in &self.entries {
                    match array_index(ek) {
                        Some(en) if en < kn => pos += 1,
                        _ => break, // 더 큰 정수키 또는 문자열키 → 여기 삽입
                    }
                }
                pos
            }
            None => self.entries.len(),
        }
    }
    pub fn insert(&mut self, k: String, v: Value) -> Option<Value> {
        if let Some(&i) = self.index.get(&k) {
            return Some(std::mem::replace(&mut self.entries[i].1, v));
        }
        let pos = self.insert_position(&k);
        self.entries.insert(pos, (k, v));
        for i in pos..self.entries.len() {
            self.index.insert(self.entries[i].0.clone(), i);
        }
        None
    }
    pub fn remove(&mut self, k: &str) -> Option<Value> {
        let &i = self.index.get(k)?;
        let (_, v) = self.entries.remove(i);
        self.index.remove(k);
        for j in i..self.entries.len() {
            self.index.insert(self.entries[j].0.clone(), j);
        }
        Some(v)
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.iter().map(|(k, _)| k)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }
}

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    // BigInt (임의 정밀도 정수). Number 와 섞어 산술하면 TypeError (표준 §6.1.6.2).
    BigInt(Rc<crate::js::bigint::BigInt>),
    Str(String),
    Obj(Rc<RefCell<ObjMap>>),
    // 배열은 항목 + own-property 맵을 가진 객체(표준). arr.push 재정의 등 지원.
    Arr(Rc<ArrayObj>),
    Fn(Rc<JsFn>),
    Native(Native),
    // DOM 요소 핸들: 아레나 NodeId (구조 변형에도 안정)
    Dom(crate::dom::NodeId),
    Class(Rc<JsClass>),
    Instance(Rc<Instance>),
    // bind 로 만든 바운드 함수: (대상, this, 선행 인자)
    Bound(Rc<(Value, Value, Vec<Value>)>),
    // 접근자 프로퍼티(get/set). 객체 맵에만 저장된다. 읽기는 get 을 호출하고,
    // 대입은 set 을 호출한다(없으면 각각 undefined / 무시). 다른 경로엔 노출되지 않음.
    Accessor(Rc<AccessorPair>),
    // Map/Set — 삽입 순서 보존. 키 비교는 strict_eq (객체는 참조 동일).
    MapVal(Rc<RefCell<Vec<(Value, Value)>>>),
    SetVal(Rc<RefCell<Vec<Value>>>),
    // element.style — 요소의 inline style 속성에 대한 라이브 프록시(CSSStyleDeclaration).
    Style(crate::dom::NodeId),
    // el.dataset — **살아있는 뷰**다. 예전엔 스냅샷 객체라 el.dataset.x = '1' 이
    // 조용히 사라졌다 (속성이 안 바뀐다).
    Dataset(crate::dom::NodeId),
    // element.classList — 요소의 class 속성에 대한 라이브 프록시(DOMTokenList).
    ClassList(crate::dom::NodeId),
    // Attr 노드 (§4.9.2): (소유 요소, 정규화된 이름) 에 대한 **살아 있는** 뷰다.
    // 예전엔 el.attributes 가 평범한 {name, value} 객체를 줬다 — value 를 바꿔도
    // 요소에 반영되지 않았고 ownerElement 도 없었다 (조용히 아무 일도 안 함).
    Attr(crate::dom::NodeId, String),
    // CSSOM (§CSSOM 6): 스타일시트와 규칙에 대한 살아 있는 뷰.
    // Sheet(시트 인덱스) / CssRule(시트, 규칙) / RuleStyle(시트, 규칙)
    Sheet(usize),
    CssRule(usize, usize),
    RuleStyle(usize, usize),
    // new Proxy(target, handler) — get/set/has 트랩 지원 (프레임워크 반응성).
    Proxy(Rc<(Value, Value)>),
    // function* 로 만든 지연 제너레이터. 호출 시 즉시 평가하지 않고, next()마다 다음
    // yield 까지 본문을 재개 실행한다(무한 제너레이터/양방향 next(v) 지원). generator.rs.
    Gen(Rc<RefCell<GenState>>),
    // Symbol 원시값. key 는 프로퍼티 키로 쓰일 때의 문자열(잘 알려진 심볼은 "\u{0}@@iterator"
    // 등 고정, 일반 심볼은 "\u{0}@@sym:<n>" 고유). 동일성(===)은 key 비교. desc 는 설명.
    Symbol(Rc<SymbolData>),
    // getComputedStyle(el) 이 돌려주는 읽기전용 계산 스타일 뷰. 요소 NodeId 로
    // computed_styles 맵을 조회한다(카멜케이스/대시 프로퍼티 + getPropertyValue).
    ComputedStyle(crate::dom::NodeId),
}

// 접근자 프로퍼티: get/set 함수 쌍. 둘 중 하나만 있을 수 있다.
// (예전엔 getter 만 있고 setter 는 파싱 후 버려져, 대입이 setter 를 조용히 우회했다)
pub struct AccessorPair {
    pub get: Option<Value>,
    pub set: Option<Value>,
}

impl AccessorPair {
    pub fn getter(g: Value) -> Rc<AccessorPair> {
        Rc::new(AccessorPair { get: Some(g), set: None })
    }
}

// Symbol 원시값의 데이터. key 로 프로퍼티 저장 키와 동일성을 동시에 표현한다.
pub struct SymbolData {
    pub key: String,
    pub desc: Option<String>,
}

// 배열 객체: 인덱스 항목(items)과 own-property(props)를 분리 보관.
// borrow()/borrow_mut() 는 items 로 위임 — 기존 접근 코드가 그대로 동작한다.
// props 는 arr.push=fn 재정의나 arr.customProp=x 같은 표준 동작을 위한 것.
pub struct ArrayObj {
    items: RefCell<Vec<Value>>,
    props: RefCell<HashMap<String, Value>>,
}

impl ArrayObj {
    pub fn new(items: Vec<Value>) -> Rc<ArrayObj> {
        Rc::new(ArrayObj { items: RefCell::new(items), props: RefCell::new(HashMap::new()) })
    }
    pub fn borrow(&self) -> std::cell::Ref<'_, Vec<Value>> {
        self.items.borrow()
    }
    pub fn borrow_mut(&self) -> std::cell::RefMut<'_, Vec<Value>> {
        self.items.borrow_mut()
    }
    pub fn get_prop(&self, k: &str) -> Option<Value> {
        self.props.borrow().get(k).cloned()
    }
    pub fn set_prop(&self, k: String, v: Value) {
        self.props.borrow_mut().insert(k, v);
    }
    // 인덱스 외의 own 프로퍼티 (엔진 내부 마커 제외) — Object.assign 등의 열거용
    pub fn own_props(&self) -> Vec<(String, Value)> {
        self.props
            .borrow()
            .iter()
            .filter(|(k, _)| !is_internal_key(k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

pub struct JsFn {
    // 이 함수가 어느 클래스의 private 스코프 안에서 만들어졌는가 (0 = 없음).
    // private 이름 해석은 **렉시컬**이다 — 클래스 메서드 안에서 만든 콜백도
    // 나중에 호출되면 그 클래스의 #x 를 볼 수 있어야 한다.
    pub priv_id: std::cell::Cell<u64>,
    // 함수 이름 (§10.2.9 SetFunctionName). 선언/명명 함수식은 그 이름, 익명 함수는
    // 대입 대상 이름(NamedEvaluation)이 붙는다. 예전엔 필드 자체가 없어서 f.name 이
    // 항상 "" 였다 — 이름으로 함수를 판별하는 코드가 조용히 어긋난다.
    pub name: RefCell<String>,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    pub env: EnvRef, // 클로저가 캡처한 렉시컬 환경
    pub is_arrow: bool,
    pub is_generator: bool, // function* — 호출 시 yield 값을 모아 반복자 반환(eager)
    pub is_async: bool, // async — 반환값을 이행된 Promise 로 감싼다
    pub this: Option<Box<Value>>, // 화살표가 정의 시점에 캡처한 this
    // 이 함수가 클래스 메서드면 그 클래스의 부모 (super.x 해석용)
    // 이 함수가 클래스 메서드면 그 클래스의 부모 생성자 (super.x 해석용).
    // 클래스일 수도, 일반 생성자(Error/함수)일 수도 있어 Value 로 둔다.
    pub super_class: Option<Value>,
    // 함수도 객체: F.prototype / F.staticProp 등 (Rc 공유 → 변경 반영)
    pub props: RefCell<HashMap<String, Value>>,
}

pub struct JsClass {
    // 이 클래스 평가에서 만들어진 private 이름들의 스코프 id (§6.2.12).
    // 같은 이름 #x 라도 클래스가 다르면 **다른 private 이름**이다 — 그래서 id 가 필요하다.
    pub priv_id: u64,
    pub name: String,
    pub parent: Option<Rc<JsClass>>,
    // 클래스가 아닌 생성자를 확장한 경우(class E extends Error / extends function).
    // 표준은 아무 생성자나 확장 가능하다. super() 는 이 생성자를 실행해 this 를 채운다.
    pub parent_ctor: Option<Value>,
    pub ctor: Option<Rc<JsFn>>,
    pub methods: HashMap<String, Rc<JsFn>>,
    pub getters: HashMap<String, Rc<JsFn>>,
    // set 접근자 (this=인스턴스). 예전엔 파싱 단계에서 버려서 대입이 조용히 무시됐다.
    pub setters: HashMap<String, Rc<JsFn>>,
    // static get/set (this=클래스). static get observedAttributes 가 대표.
    pub static_getters: HashMap<String, Rc<JsFn>>,
    pub static_setters: HashMap<String, Rc<JsFn>>,
    // 인스턴스 필드 초기화 함수 (없으면 None → undefined). 생성 시 this 로 호출.
    pub fields: Vec<(String, Option<Rc<JsFn>>)>,
    pub statics: RefCell<HashMap<String, Value>>,
    // C.prototype 객체는 **한 번만** 만든다. 호출마다 새로 만들면 정체성이 흔들려
    // Object.getPrototypeOf(new C()) === C.prototype 이 거짓이 된다 —
    // regenerator/babel 런타임이 이 불변식 위에 이터레이터 체인을 세운다.
    pub proto_cache: RefCell<Option<Value>>,
}

impl JsClass {
    // 자신부터 조상까지 메서드 탐색
    pub fn find_method(&self, name: &str) -> Option<Rc<JsFn>> {
        if let Some(m) = self.methods.get(name) {
            return Some(m.clone());
        }
        self.parent.as_ref().and_then(|p| p.find_method(name))
    }

    // get 접근자 탐색 (자신 → 조상)
    pub fn find_setter(&self, name: &str) -> Option<Rc<JsFn>> {
        if let Some(s) = self.setters.get(name) {
            return Some(s.clone());
        }
        self.parent.as_ref().and_then(|p| p.find_setter(name))
    }

    pub fn find_static_setter(&self, name: &str) -> Option<Rc<JsFn>> {
        if let Some(s) = self.static_setters.get(name) {
            return Some(s.clone());
        }
        self.parent.as_ref().and_then(|p| p.find_static_setter(name))
    }

    pub fn find_static_getter(&self, name: &str) -> Option<Rc<JsFn>> {
        if let Some(g) = self.static_getters.get(name) {
            return Some(g.clone());
        }
        self.parent.as_ref().and_then(|p| p.find_static_getter(name))
    }

    pub fn find_getter(&self, name: &str) -> Option<Rc<JsFn>> {
        if let Some(g) = self.getters.get(name) {
            return Some(g.clone());
        }
        self.parent.as_ref().and_then(|p| p.find_getter(name))
    }

    // 클래스 체인을 올라가며 첫 non-class 부모 생성자를 찾는다 (extends Error 등).
    pub fn find_parent_ctor(&self) -> Option<Value> {
        if let Some(pc) = &self.parent_ctor {
            return Some(pc.clone());
        }
        self.parent.as_ref().and_then(|p| p.find_parent_ctor())
    }
}

pub struct Instance {
    pub class: Rc<JsClass>,
    pub fields: RefCell<HashMap<String, Value>>,
}


impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Num(n) => write!(f, "{}", n),
            Value::BigInt(b) => write!(f, "{}n", b),
            Value::Str(s) => write!(f, "{:?}", s),
            Value::Obj(_) => write!(f, "[object]"),
            Value::Arr(_) => write!(f, "[array]"),
            Value::Fn(_) => write!(f, "[function]"),
            Value::Native(n) => write!(f, "[native {:?}]", n),
            Value::Dom(p) => write!(f, "[dom {:?}]", p),
            Value::Class(c) => write!(f, "[class {}]", c.name),
            Value::Instance(i) => write!(f, "[instance {}]", i.class.name),
            Value::Bound(_) => write!(f, "[bound function]"),
            Value::Accessor(_) => write!(f, "[accessor]"),
            Value::MapVal(_) => write!(f, "[object Map]"),
            Value::SetVal(_) => write!(f, "[object Set]"),
            Value::Style(id) => write!(f, "[style {:?}]", id),
            Value::Attr(id, n) => write!(f, "[attr {} of {:?}]", n, id),
            Value::Sheet(i) => write!(f, "[object CSSStyleSheet #{}]", i),
            Value::CssRule(s, r) => write!(f, "[object CSSStyleRule {}:{}]", s, r),
            Value::RuleStyle(s, r) => write!(f, "[object CSSStyleDeclaration {}:{}]", s, r),
            Value::Dataset(id) => write!(f, "[dataset {:?}]", id),
            Value::ClassList(id) => write!(f, "[classList {:?}]", id),
            Value::Proxy(_) => write!(f, "[object Proxy]"),
            Value::Gen(_) => write!(f, "[object Generator]"),
            Value::Symbol(s) => write!(f, "Symbol({})", s.desc.as_deref().unwrap_or("")),
            Value::ComputedStyle(id) => write!(f, "[computedStyle {:?}]", id),
        }
    }
}

// private 이름(#x)은 **프로퍼티가 아니다** (§6.2.12 Private Names).
// 내부 키로 저장해 Object.keys / for-in / JSON 에 절대 새지 않게 한다.
// 예전엔 그냥 "#x" 라는 이름의 필드였고, Object.keys(instance) 가 ["#x"] 를 냈다 —
// JSON.stringify 로 private 데이터가 그대로 새어 나갔다.
pub fn is_private_name(k: &str) -> bool {
    k.starts_with('#')
}

// 인스턴스 필드 조회/저장에 쓸 실제 키
// private 이름의 실제 저장 키. 클래스 스코프 id 를 붙여 클래스마다 다른 이름이 되게 한다.
pub fn field_key(k: &str, priv_id: u64) -> String {
    if is_private_name(k) {
        format!("\u{0}{}@{}", k, priv_id)
    } else {
        k.to_string()
    }
}
