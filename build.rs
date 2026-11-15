fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/bivox.ico");
        res.set("ProductName", "Bivox");
        res.set("FileDescription", "Bivox — 实时语音识别与合成");
        res.set("OriginalFilename", "bivox.exe");
        res.compile()
            .expect("编译 Windows 资源失败（assets/bivox.ico）");
    }
}
