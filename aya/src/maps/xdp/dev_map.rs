//! An array of network devices.

use std::{
    borrow::{Borrow, BorrowMut},
    num::NonZeroU32,
    os::fd::{AsFd, AsRawFd},
};

use aya_obj::generated::{bpf_devmap_val, bpf_devmap_val__bindgen_ty_1};

use crate::{
    maps::{check_bounds, check_kv_size, IterableMap, MapData, MapError},
    programs::ProgramFd,
    sys::{bpf_map_lookup_elem, bpf_map_update_elem, SyscallError},
    Pod, FEATURES,
};

/// An array of network devices.
///
/// XDP programs can use this map to redirect to other network
/// devices.
///
/// # Minimum kernel version
///
/// The minimum kernel version required to use this feature is 4.14.
///
/// # Examples
/// ```no_run
/// # let mut bpf = aya::Bpf::load(&[])?;
/// use aya::maps::xdp::DevMap;
///
/// let mut devmap = DevMap::try_from(bpf.map_mut("IFACES").unwrap())?;
/// let source = 32u32;
/// let dest = 42u32;
/// devmap.set(source, dest, None, 0);
///
/// # Ok::<(), aya::BpfError>(())
/// ```
#[doc(alias = "BPF_MAP_TYPE_DEVMAP")]
pub struct DevMap<T> {
    inner: T,
}

impl<T: Borrow<MapData>> DevMap<T> {
    pub(crate) fn new(map: T) -> Result<Self, MapError> {
        let data = map.borrow();

        if FEATURES.devmap_prog_id() {
            check_kv_size::<u32, bpf_devmap_val>(data)?;
        } else {
            check_kv_size::<u32, u32>(data)?;
        }

        Ok(Self { inner: map })
    }

    /// Returns the number of elements in the array.
    ///
    /// This corresponds to the value of `bpf_map_def::max_entries` on the eBPF side.
    pub fn len(&self) -> u32 {
        self.inner.borrow().obj.max_entries()
    }

    /// Returns the target ifindex and possible program at a given index.
    ///
    /// # Errors
    ///
    /// Returns [`MapError::OutOfBounds`] if `index` is out of bounds, [`MapError::SyscallError`]
    /// if `bpf_map_lookup_elem` fails.
    pub fn get(&self, index: u32, flags: u64) -> Result<DevMapValue, MapError> {
        let data = self.inner.borrow();
        check_bounds(data, index)?;
        let fd = data.fd().as_fd();

        let value = if FEATURES.devmap_prog_id() {
            bpf_map_lookup_elem::<_, bpf_devmap_val>(fd, &index, flags).map(|value| {
                value.map(|value| DevMapValue {
                    ifindex: value.ifindex,
                    // SAFETY: map writes use fd, map reads use id.
                    // https://github.com/torvalds/linux/blob/2dde18cd1d8fac735875f2e4987f11817cc0bc2c/include/uapi/linux/bpf.h#L6228
                    prog_id: NonZeroU32::new(unsafe { value.bpf_prog.id }),
                })
            })
        } else {
            bpf_map_lookup_elem::<_, u32>(fd, &index, flags).map(|value| {
                value.map(|ifindex| DevMapValue {
                    ifindex,
                    prog_id: None,
                })
            })
        };
        value
            .map_err(|(_, io_error)| SyscallError {
                call: "bpf_map_lookup_elem",
                io_error,
            })?
            .ok_or(MapError::KeyNotFound)
    }

    /// An iterator over the elements of the array.
    pub fn iter(&self) -> impl Iterator<Item = Result<DevMapValue, MapError>> + '_ {
        (0..self.len()).map(move |i| self.get(i, 0))
    }
}

impl<T: BorrowMut<MapData>> DevMap<T> {
    /// Sets the target ifindex at index, and optionally a chained program.
    ///
    /// When redirecting using `index`, packets will be transmitted by the interface with
    /// `ifindex`.
    ///
    /// Another XDP program can be passed in that will be run before actual transmission. It can be
    /// used to modify the packet before transmission with NIC specific data (MAC address update,
    /// checksum computations, etc) or other purposes.
    ///
    /// Note that only XDP programs with the `map = "devmap"` argument can be passed. See the
    /// kernel-space `aya_bpf::xdp` for more information.
    ///
    /// # Errors
    ///
    /// Returns [`MapError::OutOfBounds`] if `index` is out of bounds, [`MapError::SyscallError`]
    /// if `bpf_map_update_elem` fails, [`MapError::ProgIdNotSupported`] if the kernel does not
    /// support program ids and one is provided.
    pub fn set(
        &mut self,
        index: u32,
        ifindex: u32,
        program: Option<&ProgramFd>,
        flags: u64,
    ) -> Result<(), MapError> {
        let data = self.inner.borrow_mut();
        check_bounds(data, index)?;
        let fd = data.fd().as_fd();

        let res = if FEATURES.devmap_prog_id() {
            let value = bpf_devmap_val {
                ifindex,
                bpf_prog: bpf_devmap_val__bindgen_ty_1 {
                    // Default is valid as the kernel will only consider fd > 0:
                    // https://github.com/torvalds/linux/blob/2dde18cd1d8fac735875f2e4987f11817cc0bc2c/kernel/bpf/devmap.c#L866
                    // https://github.com/torvalds/linux/blob/2dde18cd1d8fac735875f2e4987f11817cc0bc2c/kernel/bpf/devmap.c#L918
                    fd: program
                        .map(|prog| prog.as_fd().as_raw_fd())
                        .unwrap_or_default(),
                },
            };
            bpf_map_update_elem(fd, Some(&index), &value, flags)
        } else {
            if program.is_some() {
                return Err(MapError::ProgIdNotSupported);
            }
            bpf_map_update_elem(fd, Some(&index), &ifindex, flags)
        };

        res.map_err(|(_, io_error)| SyscallError {
            call: "bpf_map_update_elem",
            io_error,
        })?;
        Ok(())
    }
}

impl<T: Borrow<MapData>> IterableMap<u32, DevMapValue> for DevMap<T> {
    fn map(&self) -> &MapData {
        self.inner.borrow()
    }

    fn get(&self, key: &u32) -> Result<DevMapValue, MapError> {
        self.get(*key, 0)
    }
}

unsafe impl Pod for bpf_devmap_val {}

#[derive(Clone, Copy, Debug)]
pub struct DevMapValue {
    pub ifindex: u32,
    pub prog_id: Option<NonZeroU32>,
}
