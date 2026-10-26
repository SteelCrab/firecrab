package io.firecrab.qa;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class GreetingController {
    private final String greeting;

    public GreetingController(String greeting) {
        this.greeting = greeting;
    }

    @GetMapping("/greeting")
    public String greeting() {
        return greeting;
    }
}
