// macOS 메인 스레드 스택 크기 확장 (렌더/이벤트루프 경로용).
//
// 트리워킹 JS 인터프리터·재귀 백트래킹 정규식 VM·깊은 DOM/CSS 재귀는 입력 깊이에
// 비례해 재귀한다. winit EventLoop 는 메인 스레드에서 만들어야 하므로(macOS) 렌더는
// 메인 스레드에서 돌고, 기본 8MB 스택으로는 실제 사이트(google.com 등)에서 stack
// overflow 로 abort 한다. Mach-O 링커의 -stack_size 로 메인 스택을 512MB 로 키운다.
//
// link-arg-**bins** 로 최종 바이너리 링크에만 적용한다 — proc-macro/dylib 링크에는
// -stack_size 를 쓸 수 없어(빌드 실패) 전역 rustflags 로는 안 된다.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-stack_size,0x20000000");
    }
}
