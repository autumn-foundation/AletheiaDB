with open("src/experimental/echo.rs", "r") as f:
    content = f.read()

content = content.replace("impl ActivityDensityResonator {",
                          "impl ActivityDensityResonator {\n    #[allow(dead_code)]")

with open("src/experimental/echo.rs", "w") as f:
    f.write(content)
