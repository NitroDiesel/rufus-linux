using DiscUtils;
using DiscUtils.Streams;
using System.Security.Cryptography;

if (args.Length != 1 || Directory.Exists(args[0]) || File.Exists(args[0]))
    throw new ArgumentException("Supply a new output directory; existing paths are refused.");
var directory = Path.GetFullPath(args[0]);
Directory.CreateDirectory(directory);
const long capacity = 1024L * 1024 * 1024;
var parentBytes = new byte[8 * 1024 * 1024];
Array.Fill<byte>(parentBytes, 0x5a, 0, 4096);
Array.Fill<byte>(parentBytes, 0xa5, 6 * 1024 * 1024, 65536);
var childBytes = (byte[])parentBytes.Clone();
Array.Fill<byte>(childBytes, 0x3c, 512, 512);
Array.Fill<byte>(childBytes, 0x77, 4 * 1024 * 1024, 4096);

foreach (var format in new[] { "vhd", "vhdx" })
{
    var parentPath = Path.Combine(directory, $"parent.{format}");
    var childPath = Path.Combine(directory, $"child.{format}");
    using (var stream = File.Create(parentPath))
    using (VirtualDisk parent = format == "vhd"
        ? DiscUtils.Vhd.Disk.InitializeDynamic(stream, Ownership.None, capacity)
        : DiscUtils.Vhdx.Disk.InitializeDynamic(stream, Ownership.None, capacity))
    {
        parent.Content.Write(parentBytes);
    }
    using (VirtualDisk child = format == "vhd"
        ? DiscUtils.Vhd.Disk.InitializeDifferencing(childPath, parentPath)
        : DiscUtils.Vhdx.Disk.InitializeDifferencing(childPath, parentPath))
    {
        child.Content.Position = 512;
        child.Content.Write(childBytes, 512, 512);
        child.Content.Position = 4 * 1024 * 1024;
        child.Content.Write(childBytes, 4 * 1024 * 1024, 4096);
    }
    using (VirtualDisk reopened = format == "vhd"
        ? new DiscUtils.Vhd.Disk(childPath, FileAccess.Read)
        : new DiscUtils.Vhdx.Disk(childPath, FileAccess.Read))
    {
        var actual = new byte[childBytes.Length];
        reopened.Content.ReadExactly(actual);
        if (!actual.AsSpan().SequenceEqual(childBytes))
            throw new InvalidDataException($"{format}: parent/child readback mismatch");
        if (reopened.Capacity != capacity)
            throw new InvalidDataException($"{format}: capacity mismatch");
        reopened.Content.Position = capacity - 1;
        if (reopened.Content.ReadByte() != 0)
            throw new InvalidDataException($"{format}: unexpected tail data");
    }
    foreach (var path in new[] { parentPath, childPath })
        Console.WriteLine($"{Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(path))).ToLowerInvariant()}  {Path.GetFileName(path)}");
}
Console.WriteLine($"First 8 MiB decoded child SHA-256: {Convert.ToHexString(SHA256.HashData(childBytes)).ToLowerInvariant()}");
