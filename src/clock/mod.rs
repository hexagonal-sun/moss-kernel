pub mod gettime;
pub mod realtime;
pub mod settime;
pub mod timeofday;
pub mod timespec;

pub enum ClockId {
    Monotonic = 0,
    Realtime = 1,
    ProcessCpuTimeId = 2,
    ThreadCpuTimeId = 3,
    MonotonicRaw = 4,
    RealtimeCoarse = 5,
    MonotonicCoarse = 6,
    BootTime = 7,
    RealtimeAlarm = 8,
    BootTimeAlarm = 9,
    Tai = 11,
}

impl TryFrom<i32> for ClockId {
    type Error = ();

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ClockId::Monotonic),
            1 => Ok(ClockId::Realtime),
            2 => Ok(ClockId::ProcessCpuTimeId),
            3 => Ok(ClockId::ThreadCpuTimeId),
            4 => Ok(ClockId::MonotonicRaw),
            5 => Ok(ClockId::RealtimeCoarse),
            6 => Ok(ClockId::MonotonicCoarse),
            7 => Ok(ClockId::BootTime),
            8 => Ok(ClockId::RealtimeAlarm),
            9 => Ok(ClockId::BootTimeAlarm),
            11 => Ok(ClockId::Tai),
            _ => Err(()),
        }
    }
}
