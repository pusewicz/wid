# frozen_string_literal: true

class TokenizerTest < Wid::Test
  def tokenize(string)
    tokenizer = Wid::Tokenizer.new(string)
    tokens = []
    while (token = tokenizer.next_token)
      tokens << token.to_a
    end

    tokens
  end

  def test_tokenizes_new_line
    assert_equal [[:"\n"]], tokenize("\n")
  end

  def test_increments_line_number_on_new_lines
    tokenizer = Wid::Tokenizer.new("1\n2\n3\n4")

    assert_equal 1, tokenizer.next_token.line
    assert_equal 2, tokenizer.next_token.line
    assert_equal 2, tokenizer.next_token.line
    assert_equal 3, tokenizer.next_token.line
    assert_equal 3, tokenizer.next_token.line
    assert_equal 4, tokenizer.next_token.line
    assert_equal 4, tokenizer.next_token.line
  end

  def test_returns_line_number_on_unrecognized_token
    tokenizer = Wid::Tokenizer.new("?")

    assert_raises(RuntimeError, 'Unrecognized token "?" at line 1') do
      tokenizer.next_token
    end
  end

  def test_tokenizes_numbers
    %w[3 2.5].each do |number|
      assert_equal [[:NUMBER, number]], tokenize(number)
    end
  end

  def test_tokenizes_strings
    %w['string1' "string2"].each do |string|
      assert_equal [[:STRING, string]], tokenize(string)
    end
  end

  def test_tokenizes_booleans
    %w[true false].each do |bool|
      assert_equal [[:BOOL, bool]], tokenize(bool)
    end
  end

  def test_tokenizes_nil
    assert_equal [[:NIL]], tokenize("nil")
  end

  def test_tokenizes_literals
    %w[( )].each do |operator|
      assert_equal [[operator.to_sym]], tokenize(operator)
    end
  end

  def test_tokenizes_operators
    %w[+ - > >= < <= == !=].each do |operator|
      assert_equal [[operator.to_sym]], tokenize(operator)
    end
  end

  def test_tokenizes_identifiers
    %w[number puts some_fun __dir__ MY_CONSTANT chomp! empty? value=].each do |identifier|
      assert_equal [[:IDENTIFIER, identifier]], tokenize(identifier)
    end
  end
end
