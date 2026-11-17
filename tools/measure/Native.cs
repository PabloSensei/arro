using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;

// Locale-independent access to process counters. PDH counter names are localized
// on Windows, so nothing here goes through Get-Counter.
public sealed class ProcessSample
{
    public int Pid;
    public int ParentPid;
    public string Name;
    public long CreationTime;
    public bool Accessible;
    public long PrivateWorkingSet;
    public long PrivateBytes;
    public long WorkingSet;
    public ulong Cycles;
    public long CpuTime100ns;

    public string Key { get { return Pid + ":" + CreationTime; } }
}

public static class MeasureNative
{
    const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
    const uint TH32CS_SNAPPROCESS = 0x2;
    static readonly IntPtr InvalidHandle = new IntPtr(-1);

    [StructLayout(LayoutKind.Sequential)]
    struct PROCESS_MEMORY_COUNTERS_EX2
    {
        public uint cb;
        public uint PageFaultCount;
        public UIntPtr PeakWorkingSetSize;
        public UIntPtr WorkingSetSize;
        public UIntPtr QuotaPeakPagedPoolUsage;
        public UIntPtr QuotaPagedPoolUsage;
        public UIntPtr QuotaPeakNonPagedPoolUsage;
        public UIntPtr QuotaNonPagedPoolUsage;
        public UIntPtr PagefileUsage;
        public UIntPtr PeakPagefileUsage;
        public UIntPtr PrivateUsage;
        public UIntPtr PrivateWorkingSetSize;
        public ulong SharedCommitUsage;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct PROCESSENTRY32W
    {
        public uint dwSize;
        public uint cntUsage;
        public uint th32ProcessID;
        public IntPtr th32DefaultHeapID;
        public uint th32ModuleID;
        public uint cntThreads;
        public uint th32ParentProcessID;
        public int pcPriClassBase;
        public uint dwFlags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
        public string szExeFile;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr OpenProcess(uint access, bool inherit, int pid);

    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll", EntryPoint = "K32GetProcessMemoryInfo")]
    static extern bool GetProcessMemoryInfo(IntPtr process, out PROCESS_MEMORY_COUNTERS_EX2 counters, uint size);

    [DllImport("kernel32.dll")]
    static extern bool QueryProcessCycleTime(IntPtr process, out ulong cycles);

    [DllImport("kernel32.dll")]
    static extern bool QueryThreadCycleTime(IntPtr thread, out ulong cycles);

    [DllImport("kernel32.dll")]
    static extern IntPtr GetCurrentThread();

    [DllImport("kernel32.dll")]
    static extern bool GetProcessTimes(IntPtr process, out long creation, out long exit, out long kernel, out long user);

    [DllImport("kernel32.dll")]
    static extern bool GetSystemTimes(out long idle, out long kernel, out long user);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    static extern bool Process32FirstW(IntPtr snapshot, ref PROCESSENTRY32W entry);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    static extern bool Process32NextW(IntPtr snapshot, ref PROCESSENTRY32W entry);

    // Reads every process. A process we may not open keeps Accessible = false.
    public static List<ProcessSample> SnapshotAll()
    {
        var result = new List<ProcessSample>();
        IntPtr snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if (snapshot == InvalidHandle)
            throw new InvalidOperationException("CreateToolhelp32Snapshot failed");
        try
        {
            var entry = new PROCESSENTRY32W();
            entry.dwSize = (uint)Marshal.SizeOf(typeof(PROCESSENTRY32W));
            bool more = Process32FirstW(snapshot, ref entry);
            while (more)
            {
                var sample = new ProcessSample();
                sample.Pid = (int)entry.th32ProcessID;
                sample.ParentPid = (int)entry.th32ParentProcessID;
                sample.Name = entry.szExeFile;
                Fill(sample);
                result.Add(sample);
                more = Process32NextW(snapshot, ref entry);
            }
        }
        finally
        {
            CloseHandle(snapshot);
        }
        return result;
    }

    static void Fill(ProcessSample sample)
    {
        IntPtr process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, sample.Pid);
        if (process == IntPtr.Zero)
            return;
        try
        {
            long creation, exit, kernel, user;
            if (GetProcessTimes(process, out creation, out exit, out kernel, out user))
            {
                sample.CreationTime = creation;
                sample.CpuTime100ns = kernel + user;
            }
            ulong cycles;
            if (QueryProcessCycleTime(process, out cycles))
                sample.Cycles = cycles;
            PROCESS_MEMORY_COUNTERS_EX2 counters;
            uint size = (uint)Marshal.SizeOf(typeof(PROCESS_MEMORY_COUNTERS_EX2));
            counters.cb = size;
            if (GetProcessMemoryInfo(process, out counters, size))
            {
                sample.PrivateWorkingSet = (long)counters.PrivateWorkingSetSize.ToUInt64();
                sample.PrivateBytes = (long)counters.PrivateUsage.ToUInt64();
                sample.WorkingSet = (long)counters.WorkingSetSize.ToUInt64();
                sample.Accessible = true;
            }
        }
        finally
        {
            CloseHandle(process);
        }
    }

    // Returns the root and its live descendants. Keys already in `known` stay in the
    // tree even when their parent has exited. A child must be younger than its
    // parent, otherwise the parent pid was reused.
    public static List<ProcessSample> SelectTree(List<ProcessSample> all, int rootPid, HashSet<string> known)
    {
        var byPid = new Dictionary<int, ProcessSample>();
        foreach (var sample in all)
            byPid[sample.Pid] = sample;
        var inTree = new Dictionary<int, ProcessSample>();
        ProcessSample root;
        if (byPid.TryGetValue(rootPid, out root))
            inTree[rootPid] = root;
        foreach (var sample in all)
            if (known.Contains(sample.Key))
                inTree[sample.Pid] = sample;
        bool grew = true;
        while (grew)
        {
            grew = false;
            foreach (var sample in all)
            {
                ProcessSample parent;
                if (!inTree.ContainsKey(sample.Pid)
                    && inTree.TryGetValue(sample.ParentPid, out parent)
                    && sample.CreationTime >= parent.CreationTime)
                {
                    inTree[sample.Pid] = sample;
                    grew = true;
                }
            }
        }
        foreach (var sample in inTree.Values)
            known.Add(sample.Key);
        return new List<ProcessSample>(inTree.Values);
    }

    // Busy and total time across all logical processors, in 100 ns units.
    public static long[] SystemTimes()
    {
        long idle, kernel, user;
        if (!GetSystemTimes(out idle, out kernel, out user))
            throw new InvalidOperationException("GetSystemTimes failed");
        return new long[] { kernel + user - idle, kernel + user };
    }

    // The cycle counters do not document their unit. Spinning on one thread gives the
    // rate that converts them to seconds of one core. Preemption only lowers a run, so
    // the highest of several runs is the closest.
    public static double CalibrateCyclesPerSecond()
    {
        double best = 0;
        for (int run = 0; run < 5; run++)
        {
            ulong start, end;
            IntPtr thread = GetCurrentThread();
            var watch = Stopwatch.StartNew();
            QueryThreadCycleTime(thread, out start);
            while (watch.ElapsedMilliseconds < 50) { }
            QueryThreadCycleTime(thread, out end);
            double rate = (end - start) / watch.Elapsed.TotalSeconds;
            if (rate > best)
                best = rate;
        }
        return best;
    }
}
