// Fixture API for devstack's integration tests. It reports what devstack is
// supposed to give it: its environment, its build id, and whether its
// background worker runs (ADR-0008).

var app = WebApplication.CreateBuilder(args).Build();
var workers = Environment.GetEnvironmentVariable("Jobs__Enabled") == "true";
// One per process: a pid can come again (Windows reuses them quickly).
var instance = Guid.NewGuid().ToString();

app.MapGet("/health", () => Results.Ok("ok"));
app.MapGet("/env/{name}", (string name) => Environment.GetEnvironmentVariable(name) ?? "");
app.MapGet("/info", () => new
{
    build = Environment.GetEnvironmentVariable("DEVSTACK_BUILD_ID"),
    slot = Environment.GetEnvironmentVariable("DEVSTACK_SLOT"),
    workers,
    pid = Environment.ProcessId,
    instance,
});

// Long-lived connections, for the promotion comparison (ADR-0010): which
// instance serves a stream, and when it ends.
app.MapGet("/events", async (HttpContext ctx) =>
{
    ctx.Response.Headers.ContentType = "text/event-stream";
    var id = Environment.GetEnvironmentVariable("DEVSTACK_BUILD_ID") ?? Environment.ProcessId.ToString();
    try
    {
        for (var i = 0; !ctx.RequestAborted.IsCancellationRequested; i++)
        {
            await ctx.Response.WriteAsync($"data: {id} {i}\n\n", ctx.RequestAborted);
            await ctx.Response.Body.FlushAsync(ctx.RequestAborted);
            await Task.Delay(200, ctx.RequestAborted);
        }
    }
    catch (OperationCanceledException) { }
});
app.UseWebSockets();
app.Map("/ws", async (HttpContext ctx) =>
{
    if (!ctx.WebSockets.IsWebSocketRequest)
    {
        ctx.Response.StatusCode = 400;
        return;
    }
    using var ws = await ctx.WebSockets.AcceptWebSocketAsync();
    var buffer = new byte[4096];
    while (true)
    {
        var r = await ws.ReceiveAsync(buffer, ctx.RequestAborted);
        if (r.MessageType == System.Net.WebSockets.WebSocketMessageType.Close) break;
        await ws.SendAsync(buffer.AsMemory(0, r.Count), r.MessageType, r.EndOfMessage, ctx.RequestAborted);
    }
});

app.Run();
