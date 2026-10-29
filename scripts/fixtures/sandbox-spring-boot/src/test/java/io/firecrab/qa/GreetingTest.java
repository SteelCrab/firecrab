package io.firecrab.qa;

import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.test.web.client.TestRestTemplate;

import static org.junit.jupiter.api.Assertions.assertEquals;

@SpringBootTest(webEnvironment = SpringBootTest.WebEnvironment.RANDOM_PORT)
class GreetingTest {
    @Autowired
    private TestRestTemplate client;

    @Test
    void servesConfiguredGreeting() {
        assertEquals("ok", client.getForObject("/greeting", String.class));
    }
}
