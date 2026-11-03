fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/kaols.ico");
        res.set("ProductName", "kaols");
        res.set("FileDescription", "kaols — 实时语音识别与合成");
        res.set("OriginalFilename", "kaols.exe");
        res.compile()
            .expect("编译 Windows 资源失败（assets/kaols.ico）");
    }
}
