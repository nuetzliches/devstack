// Fixture for spike (a): a generic host whose background worker records start
// and graceful stop in MARKER_DIR. "stopped" only appears if the host ran its
// regular shutdown (StopAsync), i.e. the stop signal was handled, not a kill.

var markers = Environment.GetEnvironmentVariable("MARKER_DIR") ?? ".";
var builder = Host.CreateApplicationBuilder(args);
builder.Services.AddHostedService(_ => new Worker(markers));
builder.Build().Run();

sealed class Worker(string markers) : BackgroundService
{
    protected override async Task ExecuteAsync(CancellationToken ct)
    {
        File.WriteAllText(Path.Combine(markers, "dotnet.started"), Environment.ProcessId.ToString());
        try { await Task.Delay(Timeout.Infinite, ct); } catch (OperationCanceledException) { }
    }

    public override async Task StopAsync(CancellationToken ct)
    {
        File.WriteAllText(Path.Combine(markers, "dotnet.stopped"), DateTime.UtcNow.ToString("O"));
        await base.StopAsync(ct);
    }
}
