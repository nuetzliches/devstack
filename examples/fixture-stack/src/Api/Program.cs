// Fixture API for devstack's integration tests. It reports what devstack is
// supposed to give it: its environment, its build id, and whether its
// background worker runs (ADR-0008).

var app = WebApplication.CreateBuilder(args).Build();
var workers = Environment.GetEnvironmentVariable("Jobs__Enabled") == "true";

app.MapGet("/health", () => Results.Ok("ok"));
app.MapGet("/env/{name}", (string name) => Environment.GetEnvironmentVariable(name) ?? "");
app.MapGet("/info", () => new
{
    build = Environment.GetEnvironmentVariable("DEVSTACK_BUILD_ID"),
    slot = Environment.GetEnvironmentVariable("DEVSTACK_SLOT"),
    workers,
    pid = Environment.ProcessId,
});

app.Run();
