using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.IO;

internal static class GenerateAppIcon
{
    private static readonly int[] Sizes = { 16, 20, 24, 32, 40, 48, 64, 128, 256 };

    private static byte[] RenderFrame(Image source, int size)
    {
        using (var bitmap = new Bitmap(size, size, PixelFormat.Format32bppArgb))
        using (var graphics = Graphics.FromImage(bitmap))
        using (var stream = new MemoryStream())
        {
            graphics.Clear(Color.Transparent);
            graphics.InterpolationMode = InterpolationMode.HighQualityBicubic;
            graphics.SmoothingMode = SmoothingMode.HighQuality;
            graphics.PixelOffsetMode = PixelOffsetMode.HighQuality;
            graphics.DrawImage(source, new Rectangle(0, 0, size, size));
            bitmap.Save(stream, ImageFormat.Png);
            return stream.ToArray();
        }
    }

    private static void Main(string[] args)
    {
        if (args.Length != 2)
        {
            Console.Error.WriteLine("Uso: generate-app-icon.exe <entrada.png> <saida.ico>");
            Environment.Exit(2);
        }

        var outputDirectory = Path.GetDirectoryName(Path.GetFullPath(args[1]));
        Directory.CreateDirectory(outputDirectory);

        var frames = new List<byte[]>();
        using (var source = Image.FromFile(args[0]))
        {
            foreach (var size in Sizes)
            {
                frames.Add(RenderFrame(source, size));
            }
        }

        using (var stream = File.Create(args[1]))
        using (var writer = new BinaryWriter(stream))
        {
            writer.Write((ushort)0);
            writer.Write((ushort)1);
            writer.Write((ushort)frames.Count);

            var imageOffset = 6 + (16 * frames.Count);
            for (var index = 0; index < frames.Count; index++)
            {
                var size = Sizes[index];
                var dimension = size == 256 ? (byte)0 : (byte)size;
                writer.Write(dimension);
                writer.Write(dimension);
                writer.Write((byte)0);
                writer.Write((byte)0);
                writer.Write((ushort)1);
                writer.Write((ushort)32);
                writer.Write((uint)frames[index].Length);
                writer.Write((uint)imageOffset);
                imageOffset += frames[index].Length;
            }

            foreach (var frame in frames)
            {
                writer.Write(frame);
            }
        }

        Console.WriteLine("Ícone criado: " + Path.GetFullPath(args[1]));
    }
}
