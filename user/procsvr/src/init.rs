use core::{cmp, num::NonZeroU64, slice};

use alloc::vec;
use alloc::vec::Vec;
use rille::{
    addr::VirtualConst,
    capability::{
        paging::{BasePage, Page, PageTable, PageTableFlags},
        CapRights, Captr, CaptrOps, Endpoint, Job, MessageHeader, Thread,
    },
    elf::{Elf, Segment, SegmentFlags, SegmentType},
    io_traits::{Read, Seek, SeekFrom},
};

use crate::{copy_from_ipc, Process, SegmentedCursor};

#[derive(Debug)]
pub struct Init {
    pub state: InitState,
    pub stack: Vec<InitStep>,
}

#[derive(Debug)]
pub enum InitState {
    LoadProc {
        purpose: Purpose,
        n_pages: u64,
        data: SegmentedCursor,
        proc: Option<Process>,
    },
    Nop,
}

#[derive(Debug)]
pub enum InitStep {
    Nop,
    // LoadProc
    RequestPage(usize),
    ParseElf,
    ParseSegments { total: usize },
    LoadSegments { todo: Vec<Segment> },
    StartProcess,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Purpose {
    ConsoleDriver,
}

impl Init {
    pub fn step(
        &mut self,
        hdr: MessageHeader,
        regs: [u64; 4],
        ep: Endpoint,
        job: Job,
        init: Thread,
        pgtbl: PageTable,
    ) {
        match (hdr.private(), &mut self.state, self.stack.last()) {
            (0x1, InitState::Nop, None) => {
                // Preamble: early process init

                self.state = InitState::LoadProc {
                    purpose: Purpose::ConsoleDriver,
                    n_pages: regs[1],
                    data: SegmentedCursor::default(),
                    proc: None,
                };

                self.stack.push(InitStep::ParseElf);
            }
            (0x2, InitState::LoadProc { data, .. }, Some(InitStep::RequestPage(page))) => {
                // Given requested page.
                data.inner.insert(*page, [0; 4096]);
                copy_from_ipc(data.inner.get_mut(page).unwrap());

                self.stack.pop();
            }
            (0xdead, _, _) => {
                // init is ready to die, kill it and delete it
                init.suspend().unwrap();
                init.delete().unwrap();
            }
            _ => todo!(),
        }

        'step: loop {
            match self.stack.last_mut().unwrap_or(&mut InitStep::Nop) {
                InitStep::Nop => {
                    return;
                }
                InitStep::RequestPage(pg) => {
                    let InitState::LoadProc { .. } = &mut self.state else {
                        unreachable!()
                    };
                    ep.reply_with_regs(
                        MessageHeader::new().with_length(1).with_private(0x1),
                        Captr::null(),
                        [*pg as u64, 0, 0, 0],
                    )
                    .unwrap();
                    break 'step;
                }
                InitStep::ParseElf => {
                    let InitState::LoadProc { data, proc, .. } = &mut self.state else {
                        unreachable!()
                    };
                    let Ok(elf) = Elf::parse_header(data) else {
                        self.stack.push(InitStep::RequestPage(data.pos / 4096));
                        continue 'step;
                    };

                    self.stack.pop();
                    self.stack.push(InitStep::ParseSegments {
                        total: elf.n_segments() as usize,
                    });

                    let job = Job::create(job).unwrap();
                    *proc = Some(Process {
                        job,
                        // TODO
                        thread: job.create_thread("console-driver").unwrap(),
                        pgtbl: PageTable::create().unwrap(),
                        ipc_buf: Page::create().unwrap(),
                        pages: Vec::new(),
                        elf,
                        phdrs: Vec::new(),
                        stack: Page::create().unwrap(),
                    });
                }
                InitStep::ParseSegments { total } => {
                    let InitState::LoadProc { data, proc, .. } = &mut self.state else {
                        unreachable!()
                    };
                    let elf = &mut proc.as_mut().unwrap().elf;
                    // TODO: make this more efficient
                    let Ok(mut iter) = elf.parse_segments(data) else {
                        self.stack.push(InitStep::RequestPage(data.pos / 4096));
                        continue 'step;
                    };

                    let phdrs = &mut proc.as_mut().unwrap().phdrs;

                    let len = phdrs.len();
                    for (ix, seg) in (&mut iter).enumerate() {
                        if ix < len {
                            continue;
                        }
                        let Ok(seg) = seg else {
                            drop(iter);
                            self.stack.push(InitStep::RequestPage(data.pos / 4096));
                            continue 'step;
                        };
                        phdrs.push(seg);
                    }

                    if phdrs.len() == *total {
                        self.stack.pop();
                        self.stack.push(InitStep::LoadSegments {
                            todo: phdrs
                                .iter()
                                .copied()
                                .filter(|x| x.seg_type == SegmentType::Load)
                                .collect(),
                        });
                    }
                }
                InitStep::LoadSegments { todo } => {
                    let InitState::LoadProc {
                        data,
                        proc: Some(proc),
                        ..
                    } = &mut self.state
                    else {
                        unreachable!()
                    };

                    unsafe {
                        proc.thread.configure(proc.pgtbl, proc.ipc_buf).unwrap();
                    }

                    proc.stack
                        .map(
                            proc.pgtbl,
                            VirtualConst::from_usize(0x3fffe00000),
                            PageTableFlags::RW,
                        )
                        .unwrap();

                    while let Some(seg) = todo.pop() {
                        if seg.align != 4096 {
                            todo!();
                        }
                        let base_addr = seg.virt_addr.next_multiple_of(seg.align) - seg.align;

                        // TODO: opt this so we can keep it over iterations
                        let mut file_data = vec![0; seg.file_size];
                        data.seek(SeekFrom::Start(seg.offset as u64)).unwrap();

                        let Ok(()) = data.read_exact(&mut file_data) else {
                            todo.push(seg);
                            self.stack.push(InitStep::RequestPage(data.pos / 4096));
                            continue 'step;
                        };

                        let n_pages = ((seg.virt_addr + seg.mem_size).next_multiple_of(4096)
                            - base_addr)
                            / 4096;

                        let pgs = (0..n_pages)
                            .map(|_| Page::<BasePage>::create().unwrap())
                            .collect::<Vec<_>>();
                        if seg.file_size > 0 {
                            let mut addr = seg.virt_addr;
                            let mut sz = seg.file_size;
                            let mut data_offset = 0;
                            for (n, pg) in pgs.iter().copied().enumerate() {
                                let off = addr - (base_addr + n * 4096);
                                pg.map(
                                    pgtbl,
                                    VirtualConst::from_usize(0xE000_0000),
                                    PageTableFlags::RW,
                                )
                                .unwrap();

                                let mut flags = PageTableFlags::empty();
                                if seg.flags.contains(SegmentFlags::READ) {
                                    flags |= PageTableFlags::READ;
                                }
                                if seg.flags.contains(SegmentFlags::WRITE) {
                                    flags |= PageTableFlags::WRITE;
                                }
                                if seg.flags.contains(SegmentFlags::EXEC) {
                                    flags |= PageTableFlags::EXECUTE;
                                }
                                pg.map(
                                    proc.pgtbl,
                                    VirtualConst::from_usize(base_addr + n * 4096),
                                    flags,
                                )
                                .unwrap();

                                let len = cmp::min(sz, 4096 - off);

                                // println!(
                                //     "n={:?}, addr={:#x?}, off={:#x?}, len={:?}, sz={:?}, file_sz={:?}, mem_sz={:?}",
                                //     n, addr, off, len, sz, seg.file_size, seg.mem_size
                                // );
                                unsafe {
                                    &mut slice::from_raw_parts_mut(0xE000_0000 as *mut _, 4096)
                                        [off..off + len]
                                }
                                .copy_from_slice(&file_data[data_offset..data_offset + len]);
                                addr += len;
                                sz -= len;
                                data_offset += len;
                            }
                        }
                    }

                    if todo.is_empty() {
                        self.stack.pop();
                        self.stack.push(InitStep::StartProcess);
                    }
                }
                InitStep::StartProcess => {
                    let InitState::LoadProc {
                        proc: Some(proc), ..
                    } = &mut self.state
                    else {
                        unreachable!()
                    };
                    let ep = ep.grant(CapRights::WRITE, NonZeroU64::new(2)).unwrap();

                    unsafe {
                        proc.thread
                            .start(proc.elf.entry_addr.into(), (0x4000000000u64).into(), *ep, 0)
                            .unwrap();
                    }

                    ep.delete().unwrap();
                    break 'step;
                }
            }
        }
    }
}
