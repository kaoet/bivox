//! WASAPI 进程环回采集：只抓指定进程（含其子进程）渲染出来的音频。
//!
//! 用途：侦听 VRChat 自己的声音输出（对方玩家的语音），见 `docs/decisions.md` D36。
//! 为什么不用系统环回或虚拟声卡：系统环回会把本机所有声音（包括 Bivox 自己播放的 TTS）
//! 一起采进来，会产生自听；进程环回按进程树隔离，不需要用户额外配置音频路由。
//!
//! 约束：
//! - 需要 Windows 10 2004（build 19041）及以上；
//! - 本结构持有 COM 初始化守卫，**必须在创建它的线程上 drop**（采集线程自己创建、自己释放）；
//! - 格式在 `Initialize` 时固定为 16 kHz / 单声道 / PCM16，声道混合与重采样交给 WASAPI。

use std::collections::VecDeque;
use std::mem::{ManuallyDrop, size_of};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{BLOB, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, IUnknown, Interface, Ref, implement};

/// 采集输出的采样率（与 ASR 一致）。
pub const SAMPLE_RATE: u32 = 16_000;
/// 采集输出的声道数。
pub const CHANNELS: u16 = 1;
/// 采集输出的采样位宽。
pub const BITS_PER_SAMPLE: u16 = 16;
/// 等待激活回调的最长时间。
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(8);
/// 单次等待音频事件的最长时间；超时说明这一小段时间没有新数据，不是错误。
const WAIT_TIMEOUT_MS: u32 = 50;

/// 激活回调：只负责把「完成」信号丢进同步通道，避免在 COM 回调线程里做耗时工作。
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Completion(mpsc::SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(
        &self,
        _: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        // best-effort: 接收端可能已超时退出，回调只需尽力唤醒它
        let _ = self.0.try_send(());
        Ok(())
    }
}

/// COM 初始化守卫：drop 时反初始化（与创建同线程）。
struct ComGuard;

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: 与本结构创建时的 CoInitializeEx 成对出现，且只调用一次
        unsafe {
            CoUninitialize();
        }
    }
}

/// 手写事件句柄守卫（WASAPI 用事件回调驱动采集）。
struct EventHandle(HANDLE);

impl Drop for EventHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄来自 CreateEventW，只在本结构 drop 时关闭一次
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 一次进程环回采集会话。
///
/// 只能 `Start` 一次；drop 时停止客户端。字段顺序保证 COM 反初始化发生在所有接口指针释放之后。
pub struct ProcessLoopback {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    event: EventHandle,
    _com: ComGuard,
}

impl Drop for ProcessLoopback {
    fn drop(&mut self) {
        // SAFETY: client 是本结构持有的有效接口指针
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

impl ProcessLoopback {
    /// 为 `pid`（含其子进程）建立进程环回采集并启动。
    ///
    /// `pid` 是进程树根；目标进程退出后采集会停止送包，由调用方按墙钟补充静音并检测进程存活。
    pub fn new(pid: u32) -> Result<Self> {
        // SAFETY: 本函数在采集线程上独占调用，CoInitializeEx 与 ComGuard 的 CoUninitialize 成对
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        let com = ComGuard;

        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        // VT_BLOB 借用了栈上的 params：PROPVARIANT 不能负责释放这块内存，因此用 ManuallyDrop。
        let variant = ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: (&mut params as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
                        },
                    },
                    ..Default::default()
                }),
            },
        });

        let (tx, rx) = mpsc::sync_channel(1);
        let callback: IActivateAudioInterfaceCompletionHandler = Completion(tx).into();
        // ActivateAudioInterfaceAsync 在本调用内消费激活参数；operation 自己持有完成回调。
        let operation = unsafe {
            ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(&*variant),
                &callback,
            )
        }
        .context("按进程激活音频采集失败（不会回退到全系统采集）")?;
        rx.recv_timeout(ACTIVATION_TIMEOUT)
            .context("等待 Windows 音频激活回调超时")?;

        let mut status = HRESULT(0);
        let mut unknown: Option<IUnknown> = None;
        // SAFETY: operation 是有效的 IActivateAudioInterfaceAsyncOperation
        unsafe {
            operation.GetActivateResult(&mut status, &mut unknown)?;
        }
        status.ok()?;
        let client: IAudioClient = unknown.context("音频激活未返回接口")?.cast()?;

        let format = WAVEFORMATEX {
            wFormatTag: 1, // WAVE_FORMAT_PCM
            nChannels: CHANNELS,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * u32::from(CHANNELS) * 2,
            nBlockAlign: CHANNELS * 2,
            wBitsPerSample: BITS_PER_SAMPLE,
            cbSize: 0,
        };
        // SHARED + LOOPBACK：由 WASAPI 负责混音/重采样到上面的格式。
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        // SAFETY: client 是刚激活的 IAudioClient，format 是合法的 PCM 描述
        unsafe {
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 0, 0, &format, None)?;
        }
        let event = EventHandle(unsafe { CreateEventW(None, false, false, None)? });
        // SAFETY: event 句柄有效且活到本结构 drop
        unsafe {
            client.SetEventHandle(event.0)?;
        }
        let capture = unsafe { client.GetService::<IAudioCaptureClient>()? };

        let result = Self {
            capture,
            client,
            event,
            _com: com,
        };
        // SAFETY: capture client 已绑定事件，可以开始推流
        unsafe {
            result.client.Start()?;
        }
        Ok(result)
    }

    /// 等待一次「可以读数据」或超时（超时是静音期的正常情况）。
    pub fn wait(&self) -> Result<()> {
        // SAFETY: event 句柄活到本结构 drop
        let result = unsafe { WaitForSingleObject(self.event.0, WAIT_TIMEOUT_MS) };
        ensure!(result != WAIT_FAILED, "等待音频事件失败");
        Ok(())
    }

    /// 把当前可读的包全部追加到 `queue`（PCM16 小端）。
    ///
    /// 返回 `Some(discontinuity)` 表示读到过包；`discontinuity = true` 说明 Windows 自己报告
    /// 音频有缺口，调用方应重建采集会话。返回 `None` 表示当前没有可读的包。
    pub fn read(&self, queue: &mut VecDeque<u8>) -> Result<Option<bool>> {
        // SAFETY: 读取完必须 ReleaseBuffer，下面保证无论成败都释放
        if unsafe { self.capture.GetNextPacketSize()? } == 0 {
            return Ok(None);
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let (mut frames, mut flags) = (0u32, 0u32);
        // SAFETY: 输出参数都是本栈上的有效指针
        unsafe {
            self.capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
        }
        let copied = (|| -> Result<()> {
            // 32_000 帧 = 2 秒，超过这个量说明服务端给了异常包
            ensure!(frames <= SAMPLE_RATE * 2, "异常的音频包大小：{frames} 帧");
            let bytes = frames as usize * 2;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                // 静音包允许返回空指针，必须先看标志位再碰 data。
                queue.extend(std::iter::repeat_n(0u8, bytes));
            } else if bytes > 0 {
                ensure!(!data.is_null(), "非静音音频包返回了空指针");
                // SAFETY: 数据在 ReleaseBuffer 前有效，且格式是已初始化的 PCM16 单声道
                let slice = unsafe { std::slice::from_raw_parts(data, bytes) };
                queue.extend(slice);
            }
            Ok(())
        })();
        // SAFETY: 与上面的 GetBuffer 配对，无论拷贝是否成功都要释放
        let released = unsafe { self.capture.ReleaseBuffer(frames) };
        copied?;
        released?;
        Ok(Some(
            flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0,
        ))
    }
}
